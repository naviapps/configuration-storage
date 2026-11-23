use super::*;
use crate::save_queue::*;
use std::{ffi::OsString, path::PathBuf, ptr};

struct QueueOwner(*mut ConfigurationStorageSaveQueue);

impl QueueOwner {
    fn create() -> Self {
        let owner = Self(configuration_storage_save_queue_create());
        assert!(!owner.0.is_null());
        owner
    }
}

impl Drop for QueueOwner {
    fn drop(&mut self) {
        unsafe { configuration_storage_save_queue_destroy(self.0) };
    }
}

fn assert_queue_view(queue: *const ConfigurationStorageSaveQueue, expected: [u64; 6]) {
    let view = unsafe { configuration_storage_save_queue_view(queue) };
    assert_eq!(
        [
            view.revision,
            view.committed_revision,
            view.pending_revision,
            view.writing_revision,
            view.failed_revision,
            u64::from(view.has_unsaved_changes),
        ],
        expected,
    );
}

#[test]
fn save_queue_abi_projects_revisions_and_completion_outcomes() {
    let owner = QueueOwner::create();
    let queue = owner.0;
    // The owner outlives every call. Views are copied and may outlive the queue.
    unsafe {
        assert_queue_view(queue, [0, 0, 0, 0, 0, 0]);
        assert_eq!(configuration_storage_save_queue_begin(queue), 0);
        assert_eq!(configuration_storage_save_queue_enqueue(queue), 1);
        assert_eq!(configuration_storage_save_queue_enqueue(queue), 2);
        assert_eq!(configuration_storage_save_queue_begin(queue), 2);
        assert_eq!(configuration_storage_save_queue_begin(queue), 0);
        assert_eq!(configuration_storage_save_queue_enqueue(queue), 3);
        assert_queue_view(queue, [3, 0, 3, 2, 0, 1]);

        for (revision, succeeded) in [(2, 99), (99, 0), (0, 1), (u64::MAX, 0), (2, u32::MAX)] {
            assert_eq!(
                configuration_storage_save_queue_complete(queue, revision, succeeded),
                0,
            );
            assert_queue_view(queue, [3, 0, 3, 2, 0, 1]);
        }

        assert_eq!(configuration_storage_save_queue_complete(queue, 2, 0), 3);
        assert_queue_view(queue, [3, 0, 3, 0, 0, 1]);
        assert_eq!(configuration_storage_save_queue_begin(queue), 3);
        assert_eq!(configuration_storage_save_queue_complete(queue, 3, 0), 2);
        assert_queue_view(queue, [3, 0, 3, 0, 3, 1]);
        assert_eq!(configuration_storage_save_queue_begin(queue), 3);
        assert_queue_view(queue, [3, 0, 0, 3, 0, 1]);
        assert_eq!(configuration_storage_save_queue_complete(queue, 3, 1), 1);
        assert_eq!(configuration_storage_save_queue_complete(queue, 3, 1), 0);
        assert_queue_view(queue, [3, 3, 0, 0, 0, 0]);

        assert_eq!(configuration_storage_save_queue_enqueue(queue), 4);
        assert_eq!(configuration_storage_save_queue_begin(queue), 4);
        assert_eq!(configuration_storage_save_queue_enqueue(queue), 5);
        assert_eq!(configuration_storage_save_queue_complete(queue, 4, 1), 1);
        assert_queue_view(queue, [5, 4, 5, 0, 0, 1]);
        assert_eq!(configuration_storage_save_queue_begin(queue), 5);
        assert_eq!(configuration_storage_save_queue_complete(queue, 5, 0), 2);
        assert_queue_view(queue, [5, 4, 5, 0, 5, 1]);
        assert_eq!(configuration_storage_save_queue_enqueue(queue), 6);
        assert_queue_view(queue, [6, 4, 6, 0, 0, 1]);
    }
}

#[test]
fn save_queue_abi_accepts_null_as_an_empty_unavailable_owner() {
    unsafe {
        assert_eq!(configuration_storage_save_queue_enqueue(ptr::null()), 0);
        assert_eq!(configuration_storage_save_queue_begin(ptr::null()), 0);
        assert_eq!(
            configuration_storage_save_queue_complete(ptr::null(), 1, 1),
            0
        );
        assert_queue_view(ptr::null(), [0, 0, 0, 0, 0, 0]);
        configuration_storage_save_queue_destroy(ptr::null_mut());
    }
}

#[cfg(unix)]
fn unrepresentable_recovery_path() -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from("/owned")
        .join(OsString::from_vec(vec![0xff]))
        .join("Recovery/retained")
}

#[cfg(windows)]
fn unrepresentable_recovery_path() -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    PathBuf::from(r"C:\owned")
        .join(OsString::from_wide(&[0xd800]))
        .join(r"Recovery\retained")
}

#[cfg(any(unix, windows))]
#[test]
fn unrepresentable_optional_recovery_path_keeps_the_error_without_panicking() {
    let error = Failure {
        code: ErrorCode::InvalidStoredData,
        recovery_directory: Some(unrepresentable_recovery_path()),
    };
    let owner = unsafe { Box::from_raw(result(Err(error))) };
    assert_eq!(owner.code, 1);
    let value: serde_json::Value = serde_json::from_slice(&owner.bytes).unwrap();
    assert_eq!(value["code"], "invalidStoredData");
    assert!(value.get("recoveryDirectory").is_none());
}

// Read all returned bytes while the result owner is alive. Retain the
// present/absent distinction even when a payload contains zero bytes.
fn call(
    store: &ConfigurationStorageStore,
    request: &str,
    payload: Option<&[u8]>,
) -> (u32, Value, Option<Vec<u8>>) {
    let (payload_pointer, payload_length) =
        payload.map_or((ptr::null(), 0), |bytes| (bytes.as_ptr(), bytes.len()));
    let owner = unsafe {
        Box::from_raw(configuration_storage_store_request(
            store,
            request.as_ptr(),
            request.len(),
            payload_pointer,
            payload_length,
        ))
    };
    let view = unsafe { configuration_storage_result_view_v14(&*owner) };
    let metadata = unsafe { std::slice::from_raw_parts(view.data, view.length) };
    let output = (!view.payload.is_null())
        .then(|| unsafe { std::slice::from_raw_parts(view.payload, view.payload_length).to_vec() });
    assert_eq!(view.payload_length, output.as_ref().map_or(0, Vec::len));
    (view.code, serde_json::from_slice(metadata).unwrap(), output)
}

fn open(directory: &std::path::Path, maximum_payload_bytes: usize) -> ConfigurationStorageStore {
    ConfigurationStorageStore(
        ConfigurationStore::open(Configuration {
            directory: directory.into(),
            owner: "ffi.payload.tests".into(),
            maximum_payload_bytes,
        })
        .unwrap(),
    )
}

#[test]
fn store_requests_keep_metadata_and_payload_separate_and_preserve_empty_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path(), 4);
    let load = r#"{"command":"load"}"#;
    assert_eq!(
        call(&store, load, None),
        (0, json!({"generation": null}), None)
    );

    let payload = [0, 255, 128, 0];
    let save = r#"{"command":"save","expectedGeneration":null}"#;
    let (code, metadata, bytes) = call(&store, save, Some(&payload));
    assert_eq!(code, 0);
    let generation = metadata["generation"].as_str().unwrap();
    assert_eq!(metadata, json!({"generation": generation}));
    assert_eq!(bytes.as_deref(), Some(payload.as_slice()));
    assert_eq!(call(&store, load, None), (code, metadata.clone(), bytes));

    let (code, metadata, bytes) = call(
        &store,
        &json!({"command": "save", "expectedGeneration": generation}).to_string(),
        None,
    );
    assert_eq!(code, 0);
    assert_ne!(metadata["generation"], generation);
    assert_eq!(bytes.as_deref(), Some([].as_slice()));
    let reopened = open(directory.path(), 4);
    assert_eq!(call(&reopened, load, None), (0, metadata, bytes));
}

#[test]
fn malformed_or_oversize_requests_leave_the_store_uninitialized() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path(), 1);
    let invalid = (1, json!({"code": "invalidConfiguration"}), None);
    for (request, payload) in [
        (r#"{"command":"save","payload":null}"#, None),
        (r#"{"command":"save","payload":"AA=="}"#, None),
        (r#"{"command":"load","payload":"AA=="}"#, None),
        (r#"{"command":"readBackup","payload":null}"#, None),
        (r#"{"command":"prepareRestore","payload":"AA=="}"#, None),
        (
            r#"{"command":"save","expectedGeneration":null,"token":null}"#,
            None,
        ),
        (r#"{"command":"prepareRestore","command":"save"}"#, None),
        (r#"{"command":"restoreSnapshot"}"#, None),
        (r#"{"command":"restoreSnapshot","token":null}"#, None),
        (r#"{"command":"load"}"#, Some(&[][..])),
        (r#"{"command":"load"}"#, Some(&[0][..])),
        (
            r#"{"command":"save","expectedGeneration":null}"#,
            Some(&[0, 1][..]),
        ),
        (r#"{"command":"prepareRestore"}"#, Some(&[0, 1][..])),
        (
            r#"{"command":"restoreSnapshot","token":"stale"}"#,
            Some(&[0, 1][..]),
        ),
    ] {
        assert_eq!(call(&store, request, payload), invalid, "{request}");
    }
    let overlong = " ".repeat(MAXIMUM_METADATA_BYTES + 1);
    assert_eq!(call(&store, &overlong, None), invalid);
    let request = br#"{"command":"save","expectedGeneration":null}"#;
    let owner = unsafe {
        Box::from_raw(configuration_storage_store_request(
            &store,
            request.as_ptr(),
            request.len(),
            ptr::null(),
            1,
        ))
    };
    assert_eq!(owner.code, 1);
    assert!(!directory.path().join("Generations").exists());
}

#[test]
fn backup_candidate_and_restoration_metadata_share_the_same_opaque_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path(), 32);
    let bytes = [0, 255, 128, 0, 254];
    let generation = store.0.save(None, &bytes).unwrap().generation.unwrap();

    let prepare = json!({"command": "prepareBackup", "expectedGeneration": generation}).to_string();
    let (code, review, payload) = call(&store, &prepare, None);
    assert_eq!(code, 0);
    assert_eq!(review["backup"], Value::Null);
    assert_eq!(payload, None);
    let create = json!({
        "command": "createBackup", "expectedGeneration": generation, "token": review["token"]
    });
    let (code, backup, payload) = call(&store, &create.to_string(), None);
    let snapshot = json!({"generation": generation});
    assert_eq!(code, 0);
    assert_eq!(backup["snapshot"], snapshot);
    assert!(backup["modifiedAt"].as_f64().unwrap().is_finite());
    assert_eq!(payload.as_deref(), Some(bytes.as_slice()));

    let (code, review, payload) = call(&store, &prepare, None);
    assert_eq!(code, 0);
    assert_eq!(review["backup"], backup);
    assert_eq!(payload.as_deref(), Some(bytes.as_slice()));

    let (code, page, payload) = call(
        &store,
        r#"{"command":"listSnapshotCandidates","limit":1,"source":"backup"}"#,
        None,
    );
    assert_eq!(code, 0);
    assert_eq!(payload, None);
    let reference = &page["candidates"][0];
    let request = json!({"command": "readSnapshotCandidate", "id": reference["id"]});
    let (code, candidate, payload) = call(&store, &request.to_string(), None);
    assert_eq!(code, 0);
    assert_eq!(candidate["snapshot"], snapshot);
    assert_eq!(candidate["source"], "backup");
    assert_eq!(candidate["isSelected"], true);
    assert_eq!(payload.as_deref(), Some(bytes.as_slice()));

    let (code, token, payload) = call(&store, r#"{"command":"prepareRestore"}"#, Some(&bytes));
    assert_eq!(code, 0);
    assert_eq!(payload, None);
    let request = json!({"command": "restoreSnapshot", "token": token}).to_string();
    let changed = (1, json!({"code": "restoreChanged"}), None);
    assert_eq!(call(&store, &request, Some(&[1])), changed);
    let (code, restored, payload) = call(&store, &request, Some(&bytes));
    assert_eq!(code, 0);
    assert_ne!(restored["generation"], generation);
    assert_eq!(payload.as_deref(), Some(bytes.as_slice()));
}
