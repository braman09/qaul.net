//! Peer tracking

use crate::error::PeerErrs;
use async_std::sync::{Arc, RwLock};
use std::collections::{HashMap};
use std::net::SocketAddr;
use tracing::{debug, error, info, warn};

type SourceAddr = SocketAddr;
type DstAddr = SocketAddr;

#[derive(Clone, Debug)]
pub(crate) struct Peer {
    pub id: usize,
    pub src: Option<SocketAddr>,
    pub dst: Option<SocketAddr>,
    pub known: bool,
}

impl Peer {
    pub(crate) fn link_state(&self) -> LinkState {
        use LinkState::*;
        match (self.src, self.dst) {
            (None, None) => NoLink,
            (Some(_), None) => DownlinkOnly,
            (None, Some(_)) => UplinkOnly,
            (Some(_), Some(_)) => Duplex
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkState {
    /// We're in communication with this peer
    Duplex,
    /// They can send data but not recieve it (we are the server)
    DownlinkOnly,
    /// We can send data but not receive it (they are the server)
    UplinkOnly,
    /// No communication is available over TCP
    NoLink
}

#[derive(Debug, Default)]
pub(crate) struct Peers {
    /// Lookup table by source address.
    src_to_id: RwLock<HashMap<SourceAddr, usize>>,
    /// Lookup table by destination address.
    dst_to_id: RwLock<HashMap<DstAddr, usize>>,
    /// Mapping from ID to peer data
    peers: RwLock<HashMap<usize, Peer>>,
    /// Most recently assigned peer ID
    cur: RwLock<usize>
}

impl Peers {
    /// Create a new empty peer list
    pub(crate) fn new() -> Arc<Self> {
        Default::default()
    }

    pub(crate) async fn all_known(self: &Arc<Self>) -> Vec<Peer> {
        self.peers
            .read()
            .await
            .iter()
            .map(|(_, peer)| peer.clone())
            .collect()
    }

    /// Get the state of a peer
    pub(crate) async fn peer_with_id(self: &Arc<Self>, id: usize) -> Option<Peer> {
        self.peers.read().await.get(&id).map(|v| v.clone())
    }

    /// Get a peer with the source socket address
    pub(crate) async fn peer_with_src(self: &Arc<Self>, src: &SourceAddr) -> Option<Peer> {
        if let Some(id) = self.src_to_id.read().await.get(src) {
            self.peers.read().await.get(&id).map(|v| v.clone())
        } else {
            None
        }
    }

    /// Get a peer with the destination socket address
    pub(crate) async fn peer_with_dst(self: &Arc<Self>, dst: &DstAddr) -> Option<Peer> {
        if let Some(id) = self.dst_to_id.read().await.get(dst) {
            self.peers.read().await.get(&id).map(|v| v.clone())
        } else {
            None
        }
    }

    async fn exclude_existing_peer(self: &Arc<Self>, peer: &Peer) -> Result<(), PeerInsertError> {
        if let Some(src) = peer.src {
            if self.peer_with_src(&src).await.is_some() {
                return Err(PeerInsertError::SameSrcAddr);
            }
        }
        
        if let Some(dst) = peer.dst {
            if self.peer_with_dst(&dst).await.is_some() {
                return Err(PeerInsertError::SameDstAddr);
            }
        }

        Ok(())
    }

    /// Insert a new peer, returning an error if the peer already exists. The ID will be overwritten.
    pub(crate) async fn add_peer(self: &Arc<Self>, mut peer: Peer) -> Result<usize, PeerInsertError> {
        self.exclude_existing_peer(&peer).await?;
        let mut cur = self.cur.write().await;
        *cur += 1;

        peer.id = *cur;

        if let Some(src) = peer.src {
            self.src_to_id.write().await.insert(src, *cur);
        }

        if let Some(dst) = peer.dst {
            self.dst_to_id.write().await.insert(dst, *cur);
        }

        self.peers.write().await.insert(*cur, peer);

        Ok(*cur)
    }

    pub(crate) async fn change_peer_dst(self: &Arc<Self>, id: usize, dst: &SourceAddr) -> Result<Peer, PeerInsertError> {
        if let Some(mut peer) = self.peer_with_id(id).await {
            // Get an existing peer with this destination - basically just checking if this is a trusted destination. We'll co-opt
            // this "ghost" identity.
            let ghost = self.peer_with_dst(dst).await;

            // Set the current peer's destination to the new destination
            peer.dst = Some(dst.clone());

            // Copy the "known" status from the ghost identity
            if let Some(ghost) = ghost {
                // Carry knowledge status from any existing uplink only peer
                peer.known = peer.known || ghost.known;
                // Remove both the ghost and the current peer. We will adopt its identity.
                self.del_peer(ghost.id).await;
            }
            
            // Remove old associations with the previous ID
            self.del_peer(peer.id).await;
            // Add new associations with the current ID
            self.add_peer(peer.clone()).await?;
            Ok(peer)
        } else {
            Err(PeerInsertError::NoSuchPeer)
        }
    }

    /// Remove the peer with the given ID, or do nothing if no such peer exists.
    pub(crate) async fn del_peer(
        self: &Arc<Self>,
        id: usize 
    ) {
        // Lock all data stores
        let mut peers = self.peers.write().await;
        let mut src_to_id = self.src_to_id.write().await;
        let mut dst_to_id = self.dst_to_id.write().await;

        if let Some(peer) = peers.remove(&id) { 
            if let Some(src) = peer.src {
                src_to_id.remove(&src);
            }
            if let Some(dst) = peer.dst {
                dst_to_id.remove(&dst);
            }
        }
    }

    pub(crate) async fn load<I: Into<SocketAddr>>(
        self: &Arc<Self>,
        peers: Vec<I>,
    ) -> Result<(), PeerErrs> {
        let new_peers: Vec<_> = peers.into_iter().map(Into::into).collect();

        // Lock all data stores
        let mut peers = self.peers.write().await;
        let mut dst_to_id = self.dst_to_id.write().await;
        let mut cur = self.cur.write().await;

        new_peers.into_iter().fold(Ok(()), |prev, addr| match prev {
            Ok(_) if dst_to_id.contains_key(&addr) => PeerErrs::new(addr),
            Err(e) if dst_to_id.contains_key(&addr) => Err(e.append(addr)),
            Ok(()) => {
                dst_to_id.insert(addr.clone(), *cur);
                let peer = Peer {
                    id: *cur,
                    src: None,
                    dst: Some(addr),
                    known: true,
                };
                peers.insert(*cur, peer.clone());

                *cur += 1;
                Ok(())
            }
            err @ Err(_) => {
                dst_to_id.insert(addr.clone(), *cur);
                let peer = Peer {
                    id: *cur,
                    src: None,
                    dst: Some(addr),
                    known: true,
                };
                peers.insert(*cur, peer);

                *cur += 1;
                err
            }
        })
    }

}

#[async_std::test]
async fn load_peers() {
    use std::net::{Ipv4Addr, SocketAddrV4};

    let a1 = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 8000);
    let a2 = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 9000);

    let peers = Peers::new();
    peers.load(vec![a1.clone(), a2.clone()]).await.unwrap();

    let id = peers.peer_with_dst(&a1.into()).await.unwrap().id;
    assert_eq!(peers.peer_with_id(id).await.unwrap().dst, Some(a1.into()));
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PeerInsertError {
    /// A peer with this source address already exists
    SameSrcAddr,
    /// A peer with this destination address already exists
    SameDstAddr,
    /// A peer with the requested ID was not found
    NoSuchPeer,
}
