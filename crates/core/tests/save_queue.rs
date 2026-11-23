use configuration_storage::{SaveCompletion, SnapshotSaveQueue};

#[test]
fn bursts_coalesce_and_inflight_writes_never_overlap() {
    let mut queue = SnapshotSaveQueue::default();
    assert_eq!(queue.begin(), None);
    assert_eq!(queue.enqueue(), Some(1));
    assert_eq!(queue.enqueue(), Some(2));
    assert_eq!(queue.begin(), Some(2));
    assert_eq!(queue.begin(), None);
    assert_eq!(queue.enqueue(), Some(3));
    assert_eq!(queue.enqueue(), Some(4));
    assert_eq!(queue.complete(99, true), None);
    assert_eq!(queue.complete(2, true), Some(SaveCompletion::Committed));
    assert_eq!(queue.complete(2, true), None);
    assert_eq!(queue.begin(), Some(4));
    assert_eq!(queue.complete(4, true), Some(SaveCompletion::Committed));
    assert_eq!(queue.snapshot().committed_revision, 4);
    assert!(!queue.snapshot().has_unsaved_changes());
}

#[test]
fn obsolete_failure_cannot_restore_or_discard_a_new_edit() {
    let mut queue = SnapshotSaveQueue::default();
    queue.enqueue();
    assert_eq!(queue.begin(), Some(1));
    queue.enqueue();
    assert_eq!(
        queue.complete(1, false),
        Some(SaveCompletion::SupersededFailure)
    );
    assert_eq!(queue.snapshot().failed_revision, None);
    assert_eq!(queue.begin(), Some(2));
    assert_eq!(queue.complete(2, true), Some(SaveCompletion::Committed));
    assert!(!queue.snapshot().has_unsaved_changes());
}

#[test]
fn current_failure_retains_the_same_revision_until_explicit_retry() {
    let mut queue = SnapshotSaveQueue::default();
    queue.enqueue();
    assert_eq!(queue.begin(), Some(1));
    assert_eq!(
        queue.complete(1, false),
        Some(SaveCompletion::FailedCurrent)
    );
    let failed = queue.snapshot();
    assert_eq!(failed.pending_revision, Some(1));
    assert_eq!(failed.failed_revision, Some(1));
    assert_eq!(failed.committed_revision, 0);
    assert!(failed.has_unsaved_changes());
    assert_eq!(queue.begin(), Some(1));
    assert_eq!(queue.complete(1, true), Some(SaveCompletion::Committed));
    assert_eq!(queue.snapshot().failed_revision, None);
    assert!(!queue.snapshot().has_unsaved_changes());
}
