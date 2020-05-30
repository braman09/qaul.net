use serde::{Deserialize, Serialize};

use libqaul::error::Result;
use libqaul::Identity;
use libqaul::users::UserAuth;
use libqaul::messages::{Message, MsgId};
use libqaul::helpers::{Subscription as Sub};

pub type FileId = Identity;

/// Local file abstraction
pub struct File {
    pub name: Option<String>,
    pub id: FileId,
    pub data: Option<Vec<u8>>,
    pub owner: Identity,
}

/// Describe a file's lifecycle
///
/// Not to be confused with `FileFilter`, which is part of public API
/// functions to allow users to easily filter for only certain types
/// of file data.
///
/// Filter functions then take a `Filter` and return a `Meta`.
pub enum FileMeta {
    /// Files owned by the current user
    Local(File),
    /// Network files, fully locally mirrored
    Available(File),
    /// Network files, still downloading
    InProgress {
        size: usize,
        local: usize,
        stalled: bool,
    },
    /// A network advertised file that hasn't started downloading
    Advertised { size: usize },
}

/// Describe a file's lifecycle
///
/// Filter functions for each time exist and enable
/// different sub-services based on which phase they
/// aim for.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum FileFilter {
    Local,
    Available,
    InProgress,
}

/// Interface to access files from the network
///
/// Used entirely to namespace API endpoints on `Qaul` instance,
/// without having long type identifiers.
///
/// ```norun
/// # use libqaul::{Qaul, Files};
/// # let user = unimplemented!();
/// let q = Qaul::default();
/// q.files().list(user)?;
/// ```
///
/// It's also possible to `drop` the current scope, back into the
/// primary `Qaul` scope, although this is not often useful.
///
/// ```norun
/// # use libqaul::{Qaul, Messages};
/// # let q = Qaul::default();
/// q.files().drop(); // Returns `&Qaul` again
/// ```
pub struct Files<'chain> {
    pub(crate) q: &'chain crate::Qaul,
}

/// A subscription handler that pushes out updates
pub struct Subscription {
    pub(crate) inner: Sub<Message>,
}

impl Subscription {
    pub(crate) fn new(inner: Sub<Message>) -> Self {
        Self { inner }
    }

    /// Get the next chat message
    pub async fn next(&self) -> File {
        self.inner.next().await.into()
    }
}