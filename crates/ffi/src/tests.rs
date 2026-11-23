use super::*;
use crate::save_queue::*;
use base64::{Engine, engine::general_purpose::STANDARD};
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

#[test]
fn store_payload_commands_reject_over_limit_input_before_decoding() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStore::open(Configuration {
        directory: directory.path().into(),
        owner: "ffi.payload.tests".into(),
        maximum_payload_bytes: 1,
    })
    .unwrap();
    for oversized in [
        format!(r#""{}""#, "A".repeat(32_768)),
        format!(r#""{}""#, r"\u0041".repeat(32_768)),
        r#""AAA=""#.to_owned(),
        r#""\u0041\u0041\u0041\u003d""#.to_owned(),
    ] {
        for request in [
            format!(r#"{{"command":"save","expectedGeneration":"stale","payload":{oversized}}}"#),
            format!(r#"{{"payload":{oversized},"command":"prepareRestore"}}"#),
            format!(r#"{{"command":"restoreSnapshot","payload":{oversized},"token":"stale"}}"#),
            format!(r#"["prepareRestore",{oversized}]"#),
        ] {
            assert_eq!(
                store_payload::dispatch(&store, request.as_bytes(), |_| panic!(
                    "Over-limit store request reached payload decoding"
                ))
                .unwrap_err()
                .code,
                ErrorCode::InvalidConfiguration
            );
        }
    }
    assert_eq!(store.load().unwrap().generation, None);
    assert!(!directory.path().join("Generations").exists());
}

#[test]
fn store_payload_commands_keep_wire_shapes_and_error_order() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStorageStore(
        ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "ffi.payload.tests".into(),
            maximum_payload_bytes: 1,
        })
        .unwrap(),
    );
    let send = |request: &str| {
        let result = unsafe {
            configuration_storage_store_dispatch(&store, request.as_ptr(), request.len())
        };
        let result = unsafe { Box::from_raw(result) };
        let json: serde_json::Value = serde_json::from_slice(&result.bytes).unwrap();
        (result.code, json)
    };
    for request in [
        r#"{"command":"save","payload":null}"#,
        r#"{"command":"save","payload":"AA==","token":null}"#,
        r#"{"command":"save","payload":"AA==","payload":"AA=="}"#,
        r#"{"command":"prepareRestore","command":"save","payload":"AA=="}"#,
        r#"{"command":"prepareRestore","payload":"AA==","expectedGeneration":null}"#,
        r#"{"command":"restoreSnapshot","payload":"AA=="}"#,
        r#"{"command":"restoreSnapshot","payload":"AA==","token":null}"#,
    ] {
        assert_eq!(send(request).1["code"], "invalidConfiguration");
    }
    let (code, original) = send(r#"["save",null,"AA=="]"#);
    assert_eq!(code, 0);
    assert_eq!(original["payload"], "AA==");
    let stale = "0".repeat(64);
    assert_eq!(
        send(&format!(
            r#"{{"command":"save","expectedGeneration":"{stale}","payload":"AA=="}}"#
        ))
        .1["code"],
        "conflict"
    );
    assert_eq!(
        send(r#"{"command":"restoreSnapshot","payload":"AA==","token":"stale"}"#).1["code"],
        "restoreChanged"
    );
    assert_eq!(send(r#"["load"]"#), (0, original.clone()));

    let (code, token) = send(r#"{"payload":"\/w==","comm\u0061nd":"prepare\u0052estore"}"#);
    assert_eq!(code, 0);
    let (code, restored) = send(&format!(r#"["restoreSnapshot","\u002fw==",{token}]"#));
    assert_eq!(code, 0);
    assert_eq!(restored["payload"], "/w==");
    assert_ne!(restored["generation"], original["generation"]);
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

fn dispatch(
    store: &ConfigurationStorageStore,
    command: serde_json::Value,
) -> (u32, serde_json::Value) {
    let bytes = serde_json::to_vec(&command).unwrap();
    dispatch_bytes(store, &bytes)
}

fn dispatch_bytes(store: &ConfigurationStorageStore, bytes: &[u8]) -> (u32, serde_json::Value) {
    let result =
        unsafe { configuration_storage_store_dispatch(store, bytes.as_ptr(), bytes.len()) };
    let result = unsafe { Box::from_raw(result) };
    let view = unsafe { configuration_storage_result_view(&*result) };
    let body = unsafe { std::slice::from_raw_parts(view.data, view.length) };
    (view.code, serde_json::from_slice(body).unwrap())
}

#[test]
fn both_restore_commands_retain_the_raw_primary_selector_and_database() {
    use serde_json::json;
    use std::fs;

    for from_backup in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let configuration = Configuration {
            directory: directory.path().into(),
            owner: "ffi.restore.tests".into(),
            maximum_payload_bytes: 16,
        };
        let store =
            ConfigurationStorageStore(ConfigurationStore::open(configuration.clone()).unwrap());
        let send = |command| {
            let (status, body) = dispatch(&store, command);
            assert_eq!(status, 0, "{body}");
            body
        };
        let payload = "AP+AKg==";
        let original =
            send(json!({"command": "save", "expectedGeneration": null, "payload": payload}));
        if from_backup {
            let prepared = send(json!({
                "command": "prepareBackup", "expectedGeneration": original["generation"]
            }));
            send(
                json!({"command": "createBackup", "expectedGeneration": original["generation"],
                        "token": prepared["token"]}),
            );
        }

        let review = || {
            if from_backup {
                let backup = send(json!({"command": "readBackup"}));
                assert_eq!(backup["snapshot"], original);
                backup["token"].clone()
            } else {
                send(json!({"command": "prepareRestore", "payload": payload}))
            }
        };
        let restore = |token| {
            if from_backup {
                json!({"command": "restoreBackup", "token": token})
            } else {
                json!({"command": "restoreSnapshot", "payload": payload, "token": token})
            }
        };
        let changed = if from_backup {
            "backupChanged"
        } else {
            "restoreChanged"
        };
        let stale = review();
        let selector = directory.path().join("active-generation");
        let former_id = fs::read_to_string(&selector).unwrap();
        let generations = directory.path().join("Generations");
        let former_primary = generations.join(format!("{former_id}.sqlite"));
        let original_bytes = fs::read(&former_primary).unwrap();
        let malformed = [0xff, 0, b'.', b'.', b'/', b'\n'];
        fs::write(&selector, malformed).unwrap();
        let recovery = directory.path().join("Recovery");

        assert_eq!(
            dispatch(&store, json!({"command": "load"})),
            (1, json!({"code": "invalidStoredData"}))
        );
        assert_eq!(
            dispatch(&store, restore(stale.clone())),
            (1, json!({"code": changed}))
        );
        let token = review();
        assert!(token.as_str().is_some_and(|value| !value.is_empty()));
        assert_ne!(token, stale);
        assert_eq!(fs::read(&selector).unwrap(), malformed);
        assert_eq!(fs::read(&former_primary).unwrap(), original_bytes);
        assert!(!recovery.exists());

        let command = restore(token);
        let restored = send(command.clone());
        let generation = restored["generation"].as_str().unwrap();
        assert!(!generation.is_empty());
        assert_ne!(restored["generation"], original["generation"]);
        assert_eq!(
            restored,
            json!({"generation": generation, "payload": payload})
        );
        assert_eq!(dispatch(&store, command), (1, json!({"code": changed})));
        let reopened = ConfigurationStorageStore(ConfigurationStore::open(configuration).unwrap());
        assert_eq!(
            dispatch(&reopened, json!({"command": "load"})),
            (0, restored)
        );

        let selected = fs::read_to_string(&selector).unwrap();
        assert_ne!(selected, former_id);
        assert!(generations.join(format!("{selected}.sqlite")).is_file());
        assert_eq!(fs::read(former_primary).unwrap(), original_bytes);
        let captures: Vec<_> = fs::read_dir(recovery)
            .unwrap()
            .map(|entry| entry.unwrap().path().join("active-generation"))
            .filter(|path| path.is_file())
            .collect();
        assert_eq!(captures.len(), 1);
        assert_eq!(fs::read(&captures[0]).unwrap(), malformed);
    }
}

#[test]
fn candidate_wire_pages_preserve_identity_selection_and_snapshot_bytes() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStorageStore(
        ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "ffi.candidate.tests".into(),
            maximum_payload_bytes: 16,
        })
        .unwrap(),
    );
    let send = |command: serde_json::Value| {
        let request = command.to_string();
        let (code, value) = dispatch(&store, command);
        assert_eq!(code, 0, "{request}: {value}");
        value
    };
    let list = |limit, after, source| {
        send(json!({
            "command": "listSnapshotCandidates", "limit": limit, "after": after, "source": source
        }))
    };
    assert_eq!(
        list(256, None, None),
        json!({"candidates": [], "nextCursor": null})
    );
    assert!(!directory.path().join("Generations").exists());

    let original =
        send(json!({"command": "save", "expectedGeneration": null, "payload": "gABC/g=="}));
    let preparation =
        send(json!({"command": "prepareBackup", "expectedGeneration": original["generation"]}));
    send(
        json!({"command": "createBackup", "expectedGeneration": original["generation"], "token": preparation["token"]}),
    );
    let replacement = send(json!({"command": "prepareRestore", "payload": ""}));
    let selected = send(json!({"command": "restoreSnapshot", "payload": "", "token": replacement}));

    let all = send(json!({"command": "listSnapshotCandidates", "limit": 256}));
    let references = all["candidates"].as_array().unwrap();
    assert_eq!(references.len(), 3);
    assert_eq!(all, json!({"candidates": references, "nextCursor": null}));
    let mut previous_id = "";
    let (mut generations, mut backups) = (Vec::new(), Vec::new());
    for reference in references {
        let id = reference["id"].as_str().unwrap();
        assert!(previous_id < id);
        previous_id = id;
        let source = reference["source"].as_str().unwrap();
        let selection = &reference["isSelected"];
        assert_eq!(
            *reference,
            json!({"id": id, "source": source, "isSelected": selection})
        );
        let candidate = send(json!({"command": "readSnapshotCandidate", "id": id}));
        assert!(candidate["modifiedAt"].as_f64().unwrap().is_finite());
        let expected = match (source, selection.as_bool()) {
            ("generation", Some(true)) => {
                generations.push(reference);
                &selected
            }
            ("generation", Some(false)) => {
                generations.push(reference);
                &original
            }
            ("backup", Some(true)) => {
                assert!(id.starts_with("backup:"));
                backups.push(reference);
                &original
            }
            _ => panic!("unexpected reference {reference}"),
        };
        assert_eq!(
            candidate,
            json!({"id": id, "source": source, "isSelected": selection,
                   "snapshot": expected, "modifiedAt": candidate["modifiedAt"]})
        );
    }
    assert_eq!(generations.len(), 2);
    assert_eq!(backups.len(), 1);
    let first_id = generations[0]["id"].as_str().unwrap();
    let last_id = generations[1]["id"].as_str().unwrap();
    assert_eq!(
        list(1, None, Some("generation")),
        json!({"candidates": [generations[0]], "nextCursor": first_id})
    );
    assert_eq!(
        list(1, Some(first_id), Some("generation")),
        json!({"candidates": [generations[1]], "nextCursor": null})
    );
    assert_eq!(
        list(1, Some(last_id), Some("generation")),
        json!({"candidates": [], "nextCursor": null})
    );
    assert_eq!(
        list(1, None, Some("backup")),
        json!({"candidates": backups, "nextCursor": null})
    );
    let backup_id = &backups[0]["id"];
    assert_eq!(
        dispatch(
            &store,
            json!({"command": "listSnapshotCandidates", "limit": 1,
                   "source": "generation", "after": backup_id})
        ),
        (1, json!({"code": "invalidConfiguration"}))
    );

    let read_backup = json!({"command": "readSnapshotCandidate", "id": backup_id});
    let mut unselected = send(read_backup.clone());
    unselected["isSelected"] = serde_json::Value::Null;
    std::fs::remove_file(directory.path().join("active-backup")).unwrap();
    assert_eq!(send(read_backup), unselected);
    assert_eq!(
        list(1, None, Some("backup")),
        json!({"candidates": [{"id": backup_id, "source": "backup", "isSelected": null}],
               "nextCursor": null})
    );
    assert_eq!(send(json!({"command": "load"})), selected);
}

#[test]
fn candidate_wire_rejects_invalid_requests_and_distinguishes_missing_data() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStorageStore(
        ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "ffi.candidate.tests".into(),
            maximum_payload_bytes: 16,
        })
        .unwrap(),
    );
    let invalid = (1, serde_json::json!({"code": "invalidConfiguration"}));
    for request in [
        r#"{"command":"listSnapshotCandidates"}"#,
        r#"{"command":"listSnapshotCandidates","limit":0,"after":null}"#,
        r#"{"command":"listSnapshotCandidates","limit":-1}"#,
        r#"{"command":"listSnapshotCandidates","limit":257}"#,
        r#"{"command":"listSnapshotCandidates","limit":1,"source":"unknown"}"#,
        r#"{"command":"listSnapshotCandidates","limit":1,"after":"../outside"}"#,
        r#"{"command":"listSnapshotCandidates","limit":1,"extra":null}"#,
        r#"{"command":"readSnapshotCandidate","id":null}"#,
        r#"{"command":"readSnapshotCandidate","id":"../outside"}"#,
    ] {
        assert_eq!(
            dispatch_bytes(&store, request.as_bytes()),
            invalid,
            "{request}"
        );
    }
    assert_eq!(
        dispatch_bytes(
            &store,
            br#"{"command":"readSnapshotCandidate","id":"00000000-0000-0000-0000-000000000000"}"#,
        ),
        (1, serde_json::json!({"code": "snapshotCandidateMissing"}))
    );
    for name in ["active-generation", "Generations", "Backups", "Recovery"] {
        assert!(!directory.path().join(name).exists());
    }
}

#[test]
fn backup_rotation_wire_requires_preparation_and_keeps_restore_token_separate() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStorageStore(
        ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "ffi.backup.tests".into(),
            maximum_payload_bytes: 1024,
        })
        .unwrap(),
    );
    let first = store.0.save(None, b"reviewed opaque bytes").unwrap();
    let generation = first.generation.unwrap();
    let (code, preparation) = dispatch(
        &store,
        json!({
            "command": "prepareBackup", "expectedGeneration": generation,
        }),
    );
    assert_eq!(code, 0);
    assert_eq!(preparation["backup"], serde_json::Value::Null);
    assert_eq!(preparation.as_object().unwrap().len(), 2);
    assert!(!preparation["token"].as_str().unwrap().is_empty());
    let create = json!({"command":"createBackup", "expectedGeneration":generation, "token":preparation["token"]});
    let (code, backup) = dispatch(&store, create.clone());
    assert_eq!(code, 0);
    assert_eq!(
        backup["snapshot"]["payload"],
        STANDARD.encode(b"reviewed opaque bytes")
    );
    assert_ne!(backup["token"], preparation["token"]);
    assert_eq!(dispatch(&store, create).1["code"], "backupChanged");
    let (code, preparation) = dispatch(
        &store,
        json!({"command":"prepareBackup", "expectedGeneration":generation}),
    );
    assert_eq!(code, 0);
    assert_eq!(preparation["backup"], backup);
    assert_eq!(dispatch(&store, json!({"command":"createBackup", "expectedGeneration":generation, "token":backup["token"]})).1["code"], "backupChanged");
    assert_eq!(
        dispatch(
            &store,
            json!({"command":"createBackup", "expectedGeneration":generation, "token":preparation["token"]})
        ),
        (0, backup)
    );
    store.0.save(Some(&generation), b"new primary").unwrap();
    assert_eq!(dispatch(&store, json!({"command":"createBackup", "expectedGeneration":generation, "token":preparation["token"]})).1["code"], "backupChanged");
    assert_eq!(
        dispatch(
            &store,
            json!({"command":"prepareBackup", "expectedGeneration":generation})
        )
        .1["code"],
        "conflict"
    );
}

#[test]
fn backup_wire_keeps_signed_file_dates_through_reuse_and_restore() {
    use serde_json::json;
    use std::{
        fs,
        time::{Duration, UNIX_EPOCH},
    };

    for (date, seconds) in [
        (UNIX_EPOCH - Duration::from_millis(1250), -1.25),
        (UNIX_EPOCH, 0.0),
        (UNIX_EPOCH + Duration::from_millis(2500), 2.5),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let configuration = Configuration {
            directory: directory.path().into(),
            owner: "ffi.backup.dates".into(),
            maximum_payload_bytes: 1024,
        };
        let store =
            ConfigurationStorageStore(ConfigurationStore::open(configuration.clone()).unwrap());
        let payload = STANDARD.encode([0, 255, 128, 0, 254]);
        let (code, original) = dispatch(
            &store,
            json!({"command":"save", "expectedGeneration":null, "payload":payload}),
        );
        assert_eq!(code, 0);
        assert_eq!(original["payload"], payload);
        let generation = &original["generation"];
        let prepare = json!({"command":"prepareBackup", "expectedGeneration":generation});
        let (code, preparation) = dispatch(&store, prepare.clone());
        assert_eq!(code, 0);
        let create = |token: &serde_json::Value| json!({"command":"createBackup", "expectedGeneration":generation, "token":token});
        assert_eq!(dispatch(&store, create(&preparation["token"])).0, 0);
        let selected = fs::read_to_string(directory.path().join("active-backup")).unwrap();
        let path = directory
            .path()
            .join("Backups")
            .join(format!("{selected}.sqlite"));
        let checkpoint = fs::read(&path).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(date)
            .unwrap();

        let read_backup = json!({"command":"readBackup"});
        let (code, backup) = dispatch(&store, read_backup.clone());
        assert_eq!(code, 0);
        assert_eq!(backup["snapshot"], original);
        assert_eq!(backup["modifiedAt"], seconds);
        let (code, preparation) = dispatch(&store, prepare);
        assert_eq!(code, 0);
        assert_eq!(preparation["backup"], backup);
        assert_eq!(
            dispatch(&store, create(&preparation["token"])),
            (0, backup.clone())
        );

        let (code, newer) = dispatch(
            &store,
            json!({"command":"save", "expectedGeneration":generation, "payload":"Kg=="}),
        );
        assert_eq!(code, 0);
        assert_eq!(
            dispatch(
                &store,
                json!({"command":"restoreBackup", "token":backup["token"]})
            ),
            (1, json!({"code":"backupChanged"}))
        );
        let (code, reviewed) = dispatch(&store, read_backup.clone());
        assert_eq!(code, 0);
        assert_eq!(reviewed["snapshot"], original);
        assert_eq!(reviewed["modifiedAt"], seconds);
        let (code, restored) = dispatch(
            &store,
            json!({"command":"restoreBackup", "token":reviewed["token"]}),
        );
        assert_eq!(code, 0);
        assert_eq!(restored["payload"], payload);
        assert!(restored["generation"].is_string());
        assert_ne!(restored["generation"], *generation);
        assert_ne!(restored["generation"], newer["generation"]);
        drop(store);

        let reopened =
            ConfigurationStorageStore(ConfigurationStore::open_existing(configuration).unwrap());
        assert_eq!(
            dispatch(&reopened, json!({"command":"load"})),
            (0, restored)
        );
        let (code, retained) = dispatch(&reopened, read_backup);
        assert_eq!(code, 0);
        assert_eq!(retained["snapshot"], original);
        assert_eq!(retained["modifiedAt"], seconds);
        assert_eq!(fs::read(path).unwrap(), checkpoint);
    }
}

#[test]
fn missing_null_or_unknown_rotation_fields_fail_before_native_io() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStorageStore(
        ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "ffi.backup.tests".into(),
            maximum_payload_bytes: 1024,
        })
        .unwrap(),
    );
    for command in [
        json!({"command":"prepareBackup"}),
        json!({"command":"prepareBackup", "expectedGeneration":null}),
        json!({"command":"prepareBackup", "expectedGeneration":"unused", "token":"extra"}),
        json!({"command":"createBackup", "expectedGeneration":"unused"}),
        json!({"command":"createBackup", "expectedGeneration":"unused", "token":null}),
        json!({"command":"createBackup", "expectedGeneration":"unused", "token":"wrong", "backup":null}),
    ] {
        assert_eq!(
            dispatch(&store, command),
            (1, json!({"code":"invalidConfiguration"}))
        );
    }
    assert!(!directory.path().join("Backups").exists());
    assert!(!directory.path().join("Generations").exists());
}
