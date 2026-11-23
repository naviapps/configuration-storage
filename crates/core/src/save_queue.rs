//! Opt-in write coalescing; no clock, scheduler, payload or storage policy.

/// An immutable observation of client write revisions, not on-disk generation tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveQueueSnapshot {
    pub revision: u64,
    pub committed_revision: u64,
    pub pending_revision: Option<u64>,
    pub writing_revision: Option<u64>,
    pub failed_revision: Option<u64>,
}

impl SaveQueueSnapshot {
    pub fn has_unsaved_changes(&self) -> bool {
        self.pending_revision.is_some() || self.writing_revision.is_some()
    }
}

/// Acknowledgement of one claimed write. Stale failures cannot restore old edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SaveCompletion {
    Committed = 1,
    FailedCurrent = 2,
    SupersededFailure = 3,
}

/// The adapter owns snapshots and actual writes. This reducer coalesces pending
/// revisions and never cancels an in-flight write or retries a failure itself.
#[derive(Debug, Default)]
pub struct SnapshotSaveQueue {
    state: SaveQueueSnapshot,
}

impl SnapshotSaveQueue {
    /// Allocate the next revision without reusing identities. Exhaustion leaves
    /// all state unchanged and requires the caller to retain its unsaved edit.
    pub fn enqueue(&mut self) -> Option<u64> {
        let revision = self.state.revision.checked_add(1)?;
        self.state.revision = revision;
        self.state.pending_revision = Some(revision);
        self.state.failed_revision = None;
        Some(revision)
    }

    /// Claim the latest pending revision. Only the adapter's explicit flush drains work.
    pub fn begin(&mut self) -> Option<u64> {
        if self.state.writing_revision.is_some() {
            return None;
        }
        let revision = self.state.pending_revision.take()?;
        self.state.writing_revision = Some(revision);
        self.state.failed_revision = None;
        Some(revision)
    }

    /// Acknowledge a write exactly once. A current failure remains pending for
    /// explicit retry; a superseded failure leaves the newest snapshot selected.
    pub fn complete(&mut self, revision: u64, succeeded: bool) -> Option<SaveCompletion> {
        if self.state.writing_revision != Some(revision) {
            return None;
        }
        self.state.writing_revision = None;
        if succeeded {
            self.state.committed_revision = revision;
            Some(SaveCompletion::Committed)
        } else if self.state.pending_revision.is_some() {
            Some(SaveCompletion::SupersededFailure)
        } else {
            self.state.pending_revision = Some(revision);
            self.state.failed_revision = Some(revision);
            Some(SaveCompletion::FailedCurrent)
        }
    }

    pub fn snapshot(&self) -> SaveQueueSnapshot {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_exhaustion_preserves_pending_and_inflight_work() {
        let mut queue = SnapshotSaveQueue::default();
        queue.state.revision = u64::MAX;
        queue.state.pending_revision = Some(u64::MAX);
        queue.state.writing_revision = Some(u64::MAX - 1);
        let before = queue.snapshot();
        assert_eq!(queue.enqueue(), None);
        assert_eq!(queue.snapshot(), before);
    }
}
