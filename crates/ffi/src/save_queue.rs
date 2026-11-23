use configuration_storage::SnapshotSaveQueue;
use std::sync::Mutex;

pub struct ConfigurationStorageSaveQueue(Mutex<SnapshotSaveQueue>);

#[repr(C)]
#[derive(Default)]
pub struct ConfigurationStorageSaveQueueView {
    pub revision: u64,
    pub committed_revision: u64,
    pub pending_revision: u64,
    pub writing_revision: u64,
    pub failed_revision: u64,
    pub has_unsaved_changes: u32,
}

#[unsafe(no_mangle)]
pub extern "C" fn configuration_storage_save_queue_create() -> *mut ConfigurationStorageSaveQueue {
    Box::into_raw(Box::new(ConfigurationStorageSaveQueue(Mutex::new(
        SnapshotSaveQueue::default(),
    ))))
}

/// # Safety
/// Owner stays live. Zero rejects null/exhausted state without changing pending work.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_save_queue_enqueue(
    owner: *const ConfigurationStorageSaveQueue,
) -> u64 {
    unsafe { owner.as_ref() }
        .and_then(|q| q.0.lock().unwrap_or_else(|e| e.into_inner()).enqueue())
        .unwrap_or(0)
}

/// # Safety
/// Owner stays live. Zero means no claim; no write is cancelled or implicitly retried.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_save_queue_begin(
    owner: *const ConfigurationStorageSaveQueue,
) -> u64 {
    unsafe { owner.as_ref() }
        .and_then(|q| q.0.lock().unwrap_or_else(|e| e.into_inner()).begin())
        .unwrap_or(0)
}

/// # Safety
/// Owner stays live. succeeded is 0/1. Result: 0 invalid, 1 committed,
/// 2 current failure retained, 3 older failure superseded by a pending edit.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_save_queue_complete(
    owner: *const ConfigurationStorageSaveQueue,
    revision: u64,
    succeeded: u32,
) -> u32 {
    if succeeded > 1 {
        return 0;
    }
    unsafe { owner.as_ref() }
        .and_then(|q| {
            q.0.lock()
                .unwrap_or_else(|e| e.into_inner())
                .complete(revision, succeeded == 1)
        })
        .map_or(0, |v| v as u32)
}

/// # Safety
/// Owner stays live. Revision zero means absence; null produces an empty view.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_save_queue_view(
    owner: *const ConfigurationStorageSaveQueue,
) -> ConfigurationStorageSaveQueueView {
    let Some(owner) = (unsafe { owner.as_ref() }) else {
        return ConfigurationStorageSaveQueueView::default();
    };
    let state = owner.0.lock().unwrap_or_else(|e| e.into_inner()).snapshot();
    ConfigurationStorageSaveQueueView {
        revision: state.revision,
        committed_revision: state.committed_revision,
        pending_revision: state.pending_revision.unwrap_or(0),
        writing_revision: state.writing_revision.unwrap_or(0),
        failed_revision: state.failed_revision.unwrap_or(0),
        has_unsaved_changes: u32::from(state.has_unsaved_changes()),
    }
}

/// # Safety
/// Null or exclusively owned, with no concurrent calls or live references.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_save_queue_destroy(
    owner: *mut ConfigurationStorageSaveQueue,
) {
    if !owner.is_null() {
        drop(unsafe { Box::from_raw(owner) });
    }
}
