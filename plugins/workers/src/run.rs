//! Invariant (§10): `ask()` is not a side channel. A worker's question surfaces as WAKE-CLASS
//! mail on the SPAWNER's lane — a durable inbox splice like any other — and then blocks the
//! worker or ends it, per the configured mode.

use tokio_util::sync::CancellationToken;

use crate::error::WorkerError;
use crate::ids::WorkerId;

/// What `ask()` came back with.
#[derive(Clone, Debug)]
pub enum AskAnswer {
    /// The spawner answered.
    Answered(String),
    /// Mode `end`, or the spawner went away: the worker stops here.
    Ended,
}

/// A live run, from the provider's side.
#[derive(Clone)]
pub struct WorkerRun {
    /// WP-6 fills this in.
    pub(crate) _id: WorkerId,
}

impl WorkerRun {
    /// WP-6.
    pub fn id(&self) -> &WorkerId {
        &self._id
    }
    /// WP-6.
    pub fn cancel(&self) -> CancellationToken {
        todo!("WP-6")
    }
    /// §10: surfaces as WAKE-CLASS mail on the SPAWNER's lane, and blocks or ends per `ask_mode`.
    ///
    /// WP-6.
    pub async fn ask(&self, _question: String) -> Result<AskAnswer, WorkerError> {
        todo!("WP-6: splice wake-class mail onto the spawner, then block or end")
    }
}
