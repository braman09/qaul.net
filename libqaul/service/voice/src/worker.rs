use {
    async_std::sync::RwLock,
    crate::{ASC_NAME, Call, CallId, CallMessage, Voice, CallUser, CallEvent},
    conjoiner,
    futures::sink::SinkExt,
    libqaul::{
        helpers::TagSet,
        messages::ID_LEN,
        users::UserAuth,
        Identity,
    },
    std::{
        collections::BTreeMap,
        sync::Arc,
    },
};

/// The worker that handles incoming messages for a client
#[tracing::instrument(skip(auth, voices), level = "info")]
pub(crate) async fn client_message_worker(auth: UserAuth, voices: Arc<Voice>, user: Identity) {
    // store some information about the user and add it to the voice state
    let user = Arc::new(CallUser {
        auth,
        invitation_subs: RwLock::new(Vec::new()),
        call_event_subs: RwLock::new(BTreeMap::new()),
    });
    voices.users.write().await.insert(user.auth.0, Arc::clone(&user));

    // subscribe to incoming messages
    let sub = voices.qaul
        .messages()
        .subscribe(user.auth.clone(), ASC_NAME, TagSet::empty())
        .await
        .unwrap();
    trace!("Creating message subscription!");

    loop {
        let msg = sub.next().await;

        // skip messages we've sent
        if msg.sender == user.auth.0 {
            continue;
        }

        // call id is carried in a tag so grab and deserialize that
        let id = msg.tags
            .iter()
            .filter(|tag| tag.key == "call-id")
            .filter(|tag| tag.val.len() != ID_LEN)
            .map(|tag| Identity::from_bytes(&tag.val))
            .next();
        let id: CallId = match id {
            Some(id) => id,
            None => {
                warn!("Call message recieved with no call id tag");
                continue;
            },
        };

        // next match on the message payload
        match conjoiner::deserialise(&msg.payload) {

            // if we have been invited to a call...
            Ok(CallMessage::Invitation(inv)) => {
                info!("Received invitation to {:?}", id);

                // add the call to our call database
                let mut call = Call {
                    id,
                    participants: inv.participants,
                    invitees: inv.invitees,
                };
                call.invitees.insert(user.auth.0);
                let res = voices.calls
                    .lock()
                    .await
                    .insert(user.auth.clone(), &call)
                    .await;
                if let Err(e) = res {
                    warn!("Failed to insert new call into directory (this might be due to the client exiting?): {}", e);
                }

                // then notify any invitation subscriptions that we've been invited
                let mut subs = user.invitation_subs.write().await;
                // oh how i long for `drain_filter`
                let mut i = 0;
                while i != subs.len() {
                    if let Err(_) = subs[i].send(call.clone()).await {
                        subs.remove(i);
                    } else {
                        i += 1;
                    }
                }
            },

            // if someone else has been invited to a call...
            Ok(CallMessage::InvitationSent(to)) => {
                info!("Recieved invitation notification for user {:?} on call {:?}", to, id);

                // update the call in our database
                let res = voices.calls
                    .lock()
                    .await
                    .update(user.auth.clone(), id, |mut call| {
                        call.invitees.insert(to);
                        call
                    })
                    .await;
                if let Err(e) = res {
                    warn!("Failed to update call in directory (this might be due to the client exiting?): {}", e);
                }

                // then push the event to any subscribers
                let event = CallEvent::UserInvited(to);
                if let Some(mut subs) = user.call_event_subs.write().await.get_mut(&id) {
                    let mut i = 0;
                    while i != subs.len() {
                        if let Err(_) = subs[i].send(event.clone()).await {
                            subs.remove(i);
                        } else {
                            i += 1;
                        }
                    }
                }
            },

            // if someone has joined the call...
            Ok(CallMessage::Join) => {
                // note who it is
                let joined_user = msg.sender;

                info!("Recieved join message for user {:?} on call {:?}", joined_user, id);

                // update the call in our database
                let res = voices.calls
                    .lock()
                    .await
                    .update(user.auth.clone(), id, |mut call| {
                        call.participants.insert(joined_user);
                        call.invitees.insert(joined_user);
                        call
                    })
                    .await;
                if let Err(e) = res {
                    warn!("Failed to update call in directory (this might be due to the client exiting?): {}", e);
                }

                // and notify any subscribers
                let event = CallEvent::UserJoined(joined_user);
                if let Some(mut subs) = user.call_event_subs.write().await.get_mut(&id) {
                    let mut i = 0;
                    while i != subs.len() {
                        if let Err(_) = subs[i].send(event.clone()).await {
                            subs.remove(i);
                        } else {
                            i += 1;
                        }
                    }
                }
            },

            // if a user has left a call
            Ok(CallMessage::Part) => {
                // note who they are
                let parting_user = msg.sender;

                info!("Receieved part message for user {:?} on call {:?}", parting_user, id);

                // update our call database
                let res = voices.calls
                    .lock()
                    .await
                    .update(user.auth.clone(), id, |mut call| {
                        call.participants.remove(&parting_user);
                        call.invitees.remove(&parting_user);
                        call
                    })
                    .await;
                if let Err(e) = res {
                    warn!("Failed to update call in directory (this might be due to the client exiting?): {}", e);
                }

                // then notify any subscribers
                let event = CallEvent::UserParted(parting_user);
                if let Some(mut subs) = user.call_event_subs.write().await.get_mut(&id) {
                    let mut i = 0;
                    while i != subs.len() {
                        if let Err(_) = subs[i].send(event.clone()).await {
                            subs.remove(i);
                        } else {
                            i += 1;
                        }
                    }
                }
            },
            Ok(CallMessage::Data(_)) => { unimplemented!(); },
            Err(_) => {
                warn!("Failed to deserialize message");
            }
        }
    }
}
