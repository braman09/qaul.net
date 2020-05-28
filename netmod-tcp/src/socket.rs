use crate::{Error, Mode, Packet, Peers, Peer, LinkState, Result};
use async_std::{
    io::{
        self,
        prelude::{ReadExt, WriteExt},
    },
    net::{TcpListener, TcpStream},
    stream::StreamExt,
    sync::{channel, Arc, Receiver, RwLock, Sender},
    task,
};
use bincode::{deserialize, serialize};
use byteorder::{BigEndian, ByteOrder};
use netmod::Frame;
use std::{collections::HashMap, net::SocketAddr, time::Duration};
use tracing::{debug, error, info, warn};

/// A wrapper around tcp socket logic
pub(crate) struct Socket {
    inner: TcpListener,
    uplinks: RwLock<HashMap<SocketAddr, TcpStream>>,
    port: u16,
    pub id: String,
}

impl Socket {
    pub(crate) async fn new(addr: &str, port: u16, id: &str) -> Result<Arc<Self>> {
        Ok(TcpListener::bind(format!("{}:{}", addr, port))
            .await
            .map(|inner| {
                Arc::new(Self {
                    inner,
                    uplinks: RwLock::new(HashMap::new()),
                    port,
                    id: id.into(),
                })
            })?)
    }

    /// Send a fully encoded packet to a specific addr.  At this point
    /// connection checking has already been done
    pub(crate) async fn send(self: &Arc<Self>, addr: SocketAddr, data: Frame) -> Result<()> {
        let mut stream = get_stream(&self.uplinks, addr).await;
        let ret = send_packet(&mut stream, Packet::Frame(data), &self.id)
            .await
            .map_err(|_| Error::FailedToSend );
        if ret.is_err() {
            self.uplinks.write().await.remove(&addr);
        }
        ret
    }

    pub(crate) async fn send_all(self: &Arc<Self>, data: Frame) -> Result<()> {
        let streams = self.uplinks.read().await;
        let ids: Vec<_> = streams.iter().map(|(id, _)| id).collect();
        for id in ids {
            self.send(*id, data.clone()).await?;
        }

        Ok(())
    }

    /// Start peering by sending a Hello packet
    pub(crate) async fn introduce(self: &Arc<Self>, id: usize, ip: SocketAddr) -> Option<()> {
        if self.uplinks.read().await.contains_key(&ip) {
            return None;
        }

        // In case we fail to immediately connect, we want to just
        // retry the same peer again and again, until it works.  We
        // spawn a long-running task here to accomplish this.
        let socket = Arc::clone(&self);
        task::spawn(async move {
            let mut ctr: u16 = 0;

            loop {
                let pre = match ctr {
                    0 => "".into(),
                    n => format!("[retry #{}]", n),
                };
                if socket.uplinks.read().await.contains_key(&ip) {
                    debug!("Peer '{}' (ID {}) appears to already have a connection. Ceasing reconnect loop.", ip.to_string(), id);
                    break;
                }

                debug!(
                    "{}, Attempting connection to peer '{}'",
                    pre,
                    ip.to_string()
                );
                let mut s = match TcpStream::connect(ip).await {
                    Ok(s) => {
                        debug!("Successfully connected to peer '{}'", ip.to_string());
                        s
                    }
                    Err(e) => {
                        error!(
                            "Failed to connect to peer '{}': {}",
                            ip.to_string(),
                            e.to_string()
                        );
                        task::sleep(Duration::from_secs(5)).await;
                        debug!(
                            "Retry timeout expired for peer '{}', proceeding with retry {}",
                            ip.to_string(),
                            ctr
                        );
                        continue;
                    }
                };

                socket.uplinks.write().await.insert(ip, s.clone());
                &socket.uplinks.read().await;

                match send_packet(&mut s, Packet::Hello { port: socket.port }, &socket.id).await {
                    Ok(()) => break,
                    Err(e) => {
                        error!("Failed to send packet: {}", e.to_string());
                        task::sleep(Duration::from_secs(10)).await;
                        ctr += 1;
                        continue;
                    }
                }
            }
        });

        Some(())
    }

    /// Run the async
    pub(crate) fn start(
        self: &Arc<Self>,
        mode: Mode,
        peers: &Arc<Peers>,
    ) -> Receiver<(Frame, usize)> {
        let socket = Arc::clone(&self);
        let peers = Arc::clone(peers);
        let (tx, rx) = channel(1);
        task::spawn(Self::run(tx, mode, socket, peers));
        rx
    }

    async fn run(tx: Sender<(Frame, usize)>, mode: Mode, socket: Arc<Self>, peers: Arc<Peers>) {
        let mut inc = socket.inner.incoming();

        debug!(
            "{} Listening for: {:?}",
            socket.id,
            socket.inner.local_addr()
        );
        while let Some(Ok(stream)) = inc.next().await {
            task::spawn(Socket::handle_incoming_stream(socket.clone(), stream, mode, peers.clone(), tx.clone()));
        }

        debug!("{} Exited listen loop!", socket.id);
    }

    async fn handle_incoming_stream(socket: Arc<Self>, mut stream: TcpStream, mode: Mode, peers: Arc<Peers>, tx: Sender<(Frame, usize)>) -> () {
        let src_addr = match stream.peer_addr() {
            Ok(a) => a,
            Err(e) => {
                error!("Socket '{}' unable to get peer address for incoming connection: {:?}", socket.id, e);
                return;
            },
        };

        let mut peer = match peers.peer_with_src(&src_addr).await {
            Some(p) => p,
            None => {
                match peers.add_peer(Peer {
                    id: 0,
                    src: Some(src_addr),
                    dst: None,
                    known: false
                }).await {
                    Ok(v) => {
                        match peers.peer_with_id(v).await {
                            Some(peer) => peer,
                            None => {
                                error!("Peer '{}' (src '{}') disappeared between being added and being handled. Someone removed it? Breaking.", v, src_addr);
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to record new peer with source address '{}': {:?}. This is not an anticipated condition.", src_addr, e);
                        return;
                    }
                }
            }
        };

        loop {
            debug!("At top of loop for peer '{}' (src '{}', dst '{}'). State is '{:?}'.",
                peer.id, peer.src.map(|a| a.to_string()).unwrap_or("unknown".into()), peer.dst.map(|a| a.to_string()).unwrap_or("unknown".into()), peer.link_state());
            // Try to read a packet
            let mut fb = PacketBuilder::new(&mut stream);
            if let Err(e) = fb.parse().await {
                use std::io::ErrorKind::*;
                match e.kind() {
                    UnexpectedEof => {
                        error!(
                            "Stream is at end of file, lost communication with '{}'.",
                            src_addr
                        );
                        break;
                    }
                    _ => {
                        error!(
                            "Failed to parse incoming message, dropping connection with '{}': {:?}",
                            src_addr,
                            e
                        );
                        break;
                    }
                }
            }

            let f = fb.build().unwrap();

            info!("Full packet `{:?}` received by '{}' from peer '{}'.", f, socket.id, peer.id);

            // Disambiguate differente packet types
            use LinkState::*;
            match (f, peer.link_state()) {
                // Packets coming from a peer we are in valid duplex communication with are forwarded to the next layer.
                (Packet::Frame(f), Duplex) => {
                    let id = peers.peer_with_src(&src_addr).await.unwrap().id;
                    tx.send((f, id)).await;
                }
                // Packets coming from a peer we don't have duplex communication with are junk; they need to Hello us first.
                (Packet::Frame(_), DownlinkOnly) => {
                    warn!(
                        "Dropping incoming data packet because peer {:?} is downlink only.",
                        &src_addr
                    );
                }

                // Hello from a peer we are in simplex with let us establish duplex connections
                (Packet::Hello { port }, DownlinkOnly) => {
                    let mut dst = src_addr;
                    dst.set_port(port);
                    match peers.change_peer_dst(peer.id, &dst).await {
                        Ok(p) => { peer = p },
                        Err(e) => error!("Could not update destination for peer '{}' src '{:?}' dst '{:?}' to new destination '{}': {:?}",
                            peer.id, peer.src, peer.dst, dst, e)
                    };

                    debug!("Accepted Hello from peer '{}'. New destination is '{:?}'.", peer.id, peer.dst);

                    if !peer.known && !mode.dynamic() {
                        warn!(
                            "Hello from unknown peer '{}' (src '{}') on socket '{}', closing!",
                            dst, src_addr, socket.id
                        );
                        break;
                    }                     
                    // When we have already introduced ourselves, but
                    // the peer wasn't previously verified because we
                    // hadn't gotten a message from them, we need to
                    // cancel introduction and just switch to sending
                    // a keep-alive.
                    if socket.introduce(peer.id, dst).await.is_none() {
                        let mut stream = get_stream(&socket.uplinks, dst).await;
                        match send_packet(&mut stream, Packet::KeepAlive, &socket.id).await {
                            Ok(_) => (),
                            Err(e) => {
                                error!(
                                    "Failed to send KeepAlive packet to newly introduced socket peer '{}' at '{}': {}",
                                    peer.id,
                                    dst,
                                    e.to_string()
                                );
                                break;
                            }
                        };
                    }
                }

                // Hello from a peer we are already in full duplex communication with is redundant.
                (Packet::Hello { port: _ }, Duplex) => {
                    warn!("Recieved Hello packet from an already Duplex peer at '{}' - this is odd.", src_addr);
                    let id = peers.peer_with_src(&src_addr).await.unwrap().id;
                    let mut stream = get_stream(&socket.uplinks, peer.dst.unwrap()).await;
                    match send_packet(&mut stream, Packet::KeepAlive, &socket.id).await {
                        Ok(_) => (),
                        Err(e) => error!(
                            "Failed to send KeepAlive packet to known Duplex peer ID '{}': '{}'",
                            id,
                            e.to_string()
                        ),
                    };
                }

                // Reply to a keep-alive with 2seconds delay
                (Packet::KeepAlive, UplinkOnly) | (Packet::KeepAlive, Duplex) => {
                    let peers = Arc::clone(&peers);
                    let mut stream = get_stream(&socket.uplinks, peer.dst.unwrap()).await;
                    let node_id = socket.id.clone();
                    task::spawn(async move {
                        task::sleep(Duration::from_secs(2)).await;
                        println!("{} Replying to keep-alive!", node_id);
                        match send_packet(&mut stream, Packet::KeepAlive, &node_id).await {
                            Ok(_) => (),
                            Err(e) => error!(
                                "Failed to reply to KeepAlive packet from '{}' on socket ID {}: {}",
                                src_addr,
                                node_id,
                                e.to_string()
                            ),
                        };
                    });
                }
                (packet, state) => panic!(format!("Failed with tuple: {:?}, {:?}", packet, state)),
            }
        }
        debug!("Exiting read-work loop for peer with addr '{}'.", src_addr);
    }
}

async fn get_stream(streams: &RwLock<HashMap<SocketAddr, TcpStream>>, addr: SocketAddr) -> TcpStream {
    let s = streams.read().await;
    s.get(&addr)
        .expect(&format!("No stream for addr: {}", addr))
        .clone()
}


#[tracing::instrument(skip(stream, packet), level = "debug")]
async fn send_packet(stream: &mut TcpStream, packet: Packet, id: &str) -> io::Result<()> {
    let mut vec = serialize(&packet).unwrap();
    match packet {
        Packet::Hello { .. } => debug!(
            "{} Sending HELLO to {:?}",
            id,
            stream.peer_addr()?.to_string()
        ),
        Packet::KeepAlive => debug!(
            "{} Sending KEEP-ALIVE to {:?}",
            id,
            stream.peer_addr()?.to_string()
        ),
        _ => {}
    }

    let mut buf = vec![0; 8];
    BigEndian::write_u64(&mut buf, vec.len() as u64);
    buf.append(&mut vec);

    stream.write_all(&buf).await
}

struct PacketBuilder<'s> {
    stream: &'s mut TcpStream,
    data: Option<Vec<u8>>,
}

impl<'s> PacketBuilder<'s> {
    /// Create a new frame builder from a stream
    fn new(stream: &'s mut TcpStream) -> Self {
        Self { stream, data: None }
    }

    /// Parse incoming data and initialise the builder
    async fn parse(&mut self) -> io::Result<()> {
        let mut len_buf = [0; 8];
        self.stream.read_exact(&mut len_buf).await?;
        let len = BigEndian::read_u64(&len_buf);

        let mut data_buf = vec![0; len as usize];
        self.stream.read_exact(&mut data_buf).await?;
        self.data = Some(data_buf);
        Ok(())
    }

    /// Consume the builder and maybe return a frame
    fn build(self) -> Option<Packet> {
        self.data.and_then(|vec| deserialize(&vec).ok())
    }
}

#[async_std::test]
async fn simple_send() {
    use std::net::{Ipv4Addr, SocketAddrV4};

    let s1 = Socket::new("127.0.0.1", 10010, "A =").await.unwrap();
    let s1_addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 10010);
    let p1 = Peers::new();

    let s2 = Socket::new("127.0.0.1", 10011, "B =").await.unwrap();
    let s2_addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 10011);
    let p2 = Peers::new();

    // Make p1 load p2's address, and vice versa
    p1.load(vec![s2_addr]).await.unwrap();
    p2.load(vec![s1_addr]).await.unwrap();

    s1.start(Mode::Static, &p1);
    s2.start(Mode::Static, &p2);

    // Make p1 introduce itself to p2
    let id = p1.peer_with_dst(&s2_addr.into()).await.unwrap().id;
    s1.introduce(id, s2_addr.into()).await;

    // Give the test some time to run
    task::sleep(Duration::from_secs(2)).await;

    assert_eq!(p1.peer_with_dst(&s2_addr.into()).await.unwrap().link_state(), LinkState::Duplex);
}
