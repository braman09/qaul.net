use {
    async_std::sync::RwLock,
    conjoiner,
    crate::{ASC_NAME, Result, tags, InvitationSubscription},
    futures::channel::mpsc::Sender,
    libqaul::{
        messages::{Mode, Message},
        users::UserAuth,
        Identity, Qaul,
    },
    serde::{Serialize, Deserialize},
    std::collections::{BTreeSet, BTreeMap},
};

pub type CallId = Identity;

#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    pub id: CallId,
    /// Who has joined the call?
    pub participants: BTreeSet<Identity>, 
    /// Who has been invited to the call?
    pub invitees: BTreeSet<Identity>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) enum CallMessage {
    /// invite a user
    Invitation(CallInvitation),
    /// note that you have invited a user
    InvitationSent(Identity),
    /// join a call
    Join,
    /// leave a call
    Part,
    /// send some data to the call
    Data(CallData),
}

impl CallMessage {
    /// send to a group of users
    pub(crate) async fn send_to(
        &self, 
        user: UserAuth, 
        to: &BTreeSet<Identity>,
        call: CallId,
        qaul: &Qaul,
    ) -> Result<()> {
        let messages = qaul.messages();
        let payload = conjoiner::serialise(self).unwrap(); 
        for dest in to {
            if *dest == user.0 {
                continue;
            }
            
            messages
                .send(
                    user.clone(),
                    Mode::Std(dest.clone()),
                    ASC_NAME,
                    tags::call_id(call),
                    payload.clone(),
                )
                .await?;
        }

        Ok(())
    }

    /// send to a specific user
    pub(crate) async fn send(
        &self, 
        user: UserAuth, 
        to: Identity,
        call: CallId,
        qaul: &Qaul,
    ) -> Result<()> {
        let messages = qaul.messages();
        let payload = conjoiner::serialise(self).unwrap(); 
        messages
            .send(
                user,
                Mode::Std(to),
                ASC_NAME,
                tags::call_id(call),
                payload,
            )
            .await?;

        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct CallInvitation {
    pub(crate) participants: BTreeSet<Identity>,
    pub(crate) invitees: BTreeSet<Identity>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct CallData {
    pub(crate) data: Vec<u8>,
    pub(crate) sequence_number: u64,
}

#[derive(Eq, PartialEq, Clone, Serialize, Deserialize, Debug)]
pub enum CallEvent {
    UserInvited(Identity),
    UserJoined(Identity),
    UserParted(Identity),
}

pub(crate) struct CallUser {
    pub(crate) auth: UserAuth,
    pub(crate) invitation_subs: RwLock<Vec<Sender<Call>>>,
    pub(crate) call_event_subs: RwLock<BTreeMap<CallId, Vec<Sender<CallEvent>>>>,
}
