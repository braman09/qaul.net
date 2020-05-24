//! `qaul.net` voice service

#[macro_use] extern crate tracing;

mod directory;
mod error;
mod subs;
mod types;
mod worker;

pub use self::{
    error::Result,
    subs::{InvitationSubscription, CallEventSubscription},
    types::{Call, CallId, CallEvent},
};
pub(crate) use self::{
    types::{CallMessage, CallInvitation, CallUser},
};
use {
    async_std::{
        sync::{Mutex, RwLock},
        task,
    },
    futures::{
        channel::mpsc::channel,
        future::{
            abortable,
            AbortHandle,
        },
    },
    libqaul::{
        error::Error as QaulError,
        services::ServiceEvent,
        users::UserAuth,
        Identity, Qaul,
    },
    self::directory::CallDirectory,
    std::{
        collections::{BTreeSet, BTreeMap},
        sync::{Arc, Mutex as SyncMutex},
    },
};

const ASC_NAME: &'static str = "net.qaul.voice";

pub(crate) mod tags {
    use {crate::CallId, libqaul::helpers::Tag};
    pub(crate) const CALL_LIST: &'static str = "net.qaul.voice.call_list";
    pub(crate) fn call_id(id: CallId) -> Tag {
        Tag::new("call-id", id.as_bytes().to_vec())
    }
} 

/// Voice service state
#[derive(Clone)]
pub struct Voice {
    /// libqaul instance we're running on
    pub(crate) qaul: Arc<Qaul>,
    /// where we store calls
    ///
    /// this is behind a mutex because simultaneous access to
    /// alexandria could result in race conditions
    pub(crate) calls: Arc<Mutex<CallDirectory>>,
    /// where we store user data for easy access
    pub(crate) users: Arc<RwLock<BTreeMap<Identity, Arc<CallUser>>>>,
}

impl Voice {
    /// Create a new voice service instance
    pub async fn new(qaul: Arc<Qaul>) -> Result<Arc<Self>> {
        let calls = Arc::new(Mutex::new(CallDirectory::new(qaul.clone())));
        let users = Arc::new(RwLock::new(BTreeMap::new()));
        let this = Arc::new(Self { 
            qaul, 
            calls,
            users,
        });

        // this is where we store the per-client worker abort handles to
        // allow future cleanup when the client logs out
        //
        // TODO: is this per-client or per-login...
        // if it's per-login we have to be more careful in the future
        let client_message_handles = SyncMutex::new(BTreeMap::new());
        let _this = this.clone();
        this.qaul
            .services()
            .register(ASC_NAME, move |cmd| match cmd {
                // when a client logs in...
                ServiceEvent::Open(auth) => {
                    // start up a worker
                    let (fut, handle) = abortable(worker::client_message_worker(
                        auth.clone(), 
                        _this.clone(),
                        auth.0, //this field is just to add what we want to instrument
                    ));
                    task::spawn(fut);

                    // and keep track of the abort handle for later
                    client_message_handles.lock().unwrap().insert(auth.0, handle);
                },
                // when a client logs out...
                ServiceEvent::Close(auth) => {
                    // remove the user from the user map
                    task::block_on(_this.users.write()).remove(&auth.0);
                    // and then abort the worker
                    if let Some(handle) = client_message_handles.lock().unwrap().remove(&auth.0) {
                        handle.abort();
                    }
                },
            })
            .await?;
        Ok(this)
    }

    /// Start a new call
    ///
    /// NOTE: This will not join the creating user to the call but will invite the creating
    /// user to the call.
    pub async fn start_call(&self, user: UserAuth) -> Result<CallId> {
        let call = Call {
            id: CallId::random(),
            participants: BTreeSet::new(),
            invitees: Some(user.0).into_iter().collect(), // invite ourselves to the call
        };
        let call_id = call.id.clone();
        info!("User {:?} created call {:?}", user.0, call_id);
        self.calls.lock().await.insert(user, &call).await?;
        Ok(call_id)
    }

    /// Get all calls the user knows about
    pub async fn get_calls(&self, user: UserAuth) -> Result<Vec<Call>> {
        self.calls.lock().await.get_all(user).await
    }

    /// Get a specific call by id
    pub async fn get_call(&self, user: UserAuth, id: CallId) -> Result<Call> {
        self.calls.lock().await.get(user, id).await
    }

    /// Invite a user to a call
    pub async fn invite_to_call(&self, user: UserAuth, friend: Identity, id: CallId) -> Result<()> {
        info!("{:?} is inviting {:?} to call {:?}", user.0, friend, id);
        let call = self.get_call(user.clone(), id).await?;

        // first send the invitation to the user
        let message = CallMessage::Invitation(CallInvitation {
            participants: call.participants,
            invitees: call.invitees,
        });
        message.send(user.clone(), friend, id, &self.qaul).await?;

        // then add the user to the call's invitee list
        let call = self.calls.lock().await.update(user.clone(), id, |mut call| {
            call.invitees.insert(friend);
            call
        }).await?;

        // and tell others that the user was invited to the call
        //
        // this is crucial because it allows the users of the call to 
        // completely change by the time the user joins the call and still
        // have them know who to talk to
        //
        // TODO: there's race conditions that could result in members of the
        // call having an inconsistant view of the call state
        // this could be resolved by having users occasionally randomly update
        // eachother on who they think is in the call
        let message = CallMessage::InvitationSent(friend);
        message.send_to(user.clone(), &call.invitees, id, &self.qaul).await
    }

    /// Join a call in progress
    pub async fn join_call(&self, user: UserAuth, id: CallId) -> Result<()> {
        info!("{:?} is joining call {:?}", user.0, id);

        // we join ourselves
        let call = self.calls.lock().await.update(user.clone(), id, |mut call| {
            call.participants.insert(user.0);
            call
        }).await?;

        // and notify our peers
        let message = CallMessage::Join;
        message.send_to(user, &call.invitees, id, &self.qaul).await
    }

    /// Leave a call
    pub async fn leave_call(&self, user: UserAuth, id: CallId) -> Result<()> {
        info!("{:?} is leaving call {:?}", user.0, id);

        // remove ourselves from the call
        //
        // TODO: this should actually delete the call but...
        // i don't think alexandria can do that rn
        let call = self.calls.lock().await.update(user.clone(), id, |mut call| {
            call.participants.remove(&user.0);
            call.invitees.remove(&user.0);
            call
        }).await?;

        // send a goodbye to other members of the call
        let message = CallMessage::Part;
        message.send_to(user, &call.invitees, id, &self.qaul).await
    }

    /// Subscribe to call invites
    ///
    /// NOTE: This will not notify you about the creation of your own calls, only
    /// external invites
    pub async fn subscribe_invites(&self, user: UserAuth) -> Result<InvitationSubscription> {
        // if the worker hasn't added the user we'll error because there's not much else we
        // can do
        let user = self.users.read().await.get(&user.0).ok_or(QaulError::NoUser)?.clone();

        // make the channel and add the sender to the user's subscription list
        let (sender, receiver) = channel(1);
        user.invitation_subs.write().await.push(sender);

        Ok(receiver)
    }

    /// Subscribe to call events
    ///
    /// This will notify you when a user joins, leaves, or is invited to the call
    ///
    /// NOTE: This will not notify you about locally caused call events, only events
    /// caused by external users
    pub async fn subscribe_call_events(&self, user: UserAuth, id: CallId) 
    -> Result<CallEventSubscription> {
        // if the worker hasn't added the user we'll error because there's not much else we
        // can do
        let user = self.users.read().await.get(&user.0).ok_or(QaulError::NoUser)?.clone();

        // make the channel and add it to the call's subscription list
        // if the call doesn't have one, make a new subscription list for this call
        let (sender, receiver) = channel(1);
        let mut subs = user.call_event_subs.write().await;
        if let Some(mut v) = subs.get_mut(&id) {
            v.push(sender); 
        } else {
            subs.insert(id, vec![sender]);
        }

        Ok(receiver)
    }
}
