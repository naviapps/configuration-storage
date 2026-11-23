use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn configuration(root: &Path) -> Configuration {
    Configuration {
        directory: root.to_owned(),
        owner: "com.example.fixture".into(),
        maximum_payload_bytes: 1024,
    }
}
fn setup() -> (tempfile::TempDir, ConfigurationStore) {
    let directory = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let store = ConfigurationStore::open(configuration(directory.path())).unwrap();
    (directory, store)
}
fn selected(root: &Path) -> PathBuf {
    let name = fs::read_to_string(root.join("active-generation")).unwrap();
    root.join("Generations").join(name + ".sqlite")
}
fn checkpoint_path(root: &Path) -> PathBuf {
    let marker = root.join("active-backup");
    if marker.exists() {
        let name = fs::read_to_string(marker).unwrap();
        root.join("Backups").join(format!("{name}.sqlite"))
    } else {
        root.join("Backups/snapshot.sqlite")
    }
}

#[test]
fn valid_backups_keep_signed_fractional_file_dates_without_changing_bytes() {
    use std::time::{Duration, UNIX_EPOCH};
    let (directory, store) = setup();
    let initial = store.save(None, b"accepted snapshot").unwrap();
    let backup = checkpoint(&store, initial.generation.as_deref().unwrap()).unwrap();
    let path = checkpoint_path(directory.path());
    let original = fs::read(&path).unwrap();
    for (date, seconds) in [
        (UNIX_EPOCH - Duration::from_millis(1250), -1.25),
        (UNIX_EPOCH, 0.0),
        (UNIX_EPOCH + Duration::from_millis(1250), 1.25),
    ] {
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(date)
            .unwrap();
        let read = store.read_backup().unwrap();
        assert_eq!(read.modified_at, seconds);
        assert_eq!(read.snapshot, backup.snapshot);
        assert_eq!(read.token, backup.token);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(store.load().unwrap(), initial);
    }
}

#[test]
fn explicit_restoration_retains_original_primary_selectors_for_both_restore_paths() {
    for from_backup in [false, true] {
        for marker in [
            b"damaged-selector".as_slice(),
            &[0xff, 0],
            &vec![b'x'; 4097],
        ] {
            let (directory, store) = setup();
            let initial = store.save(None, b"accepted original").unwrap();
            checkpoint(&store, initial.generation.as_deref().unwrap()).unwrap();
            let primary = selected(directory.path());
            let original = fs::read(&primary).unwrap();
            fs::write(directory.path().join("active-generation"), marker).unwrap();
            let restored = if from_backup {
                let backup = store.read_backup().unwrap();
                store.restore_backup(&backup.token).unwrap()
            } else {
                let token = store.prepare_restore(b"reviewed replacement").unwrap();
                store
                    .restore_snapshot(b"reviewed replacement", &token)
                    .unwrap()
            };
            assert_ne!(restored.generation, initial.generation);
            assert_eq!(store.load().unwrap(), restored);
            assert_eq!(fs::read(primary).unwrap(), original);
            let captures: Vec<_> = fs::read_dir(directory.path().join("Recovery"))
                .unwrap()
                .map(|entry| entry.unwrap().path().join("active-generation"))
                .filter(|path| path.exists())
                .collect();
            assert_eq!(captures.len(), 1);
            assert_eq!(fs::read(&captures[0]).unwrap(), marker);
        }
    }
}

#[test]
fn selector_capture_failure_blocks_publication_and_can_retry_without_losing_data() {
    let (directory, store) = setup();
    let initial = store.save(None, b"accepted original").unwrap();
    let primary = selected(directory.path());
    let original = fs::read(&primary).unwrap();
    let marker = fs::read(directory.path().join("active-generation")).unwrap();
    let token = store.prepare_restore(b"reviewed replacement").unwrap();
    let obstruction = directory.path().join("Recovery");
    fs::write(&obstruction, b"owned capture obstruction").unwrap();
    assert_eq!(
        failure(store.restore_snapshot(b"reviewed replacement", &token)).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(
        fs::read(directory.path().join("active-generation")).unwrap(),
        marker
    );
    assert_eq!(fs::read(&primary).unwrap(), original);
    assert_eq!(store.load().unwrap(), initial);
    assert_eq!(
        fs::read_dir(directory.path().join("Generations"))
            .unwrap()
            .count(),
        1
    );
    fs::remove_file(obstruction).unwrap();
    let restored = store
        .restore_snapshot(b"reviewed replacement", &token)
        .unwrap();
    assert_eq!(store.load().unwrap(), restored);
    assert_eq!(fs::read(primary).unwrap(), original);
}
fn failure<T>(result: Result<T, Failure>) -> Failure {
    match result {
        Err(f) => f,
        Ok(_) => panic!("expected failure"),
    }
}

fn replace_payload_without_advancing_nonce(root: &Path, payload: &[u8]) {
    let connection = rusqlite::Connection::open(selected(root)).unwrap();
    connection
        .execute(
            "UPDATE store_state SET payload=?1,digest=?2 WHERE id=1",
            rusqlite::params![payload, database::digest(payload)],
        )
        .unwrap();
}

#[test]
fn reused_nonce_does_not_allow_an_old_writer_to_overwrite_changed_bytes() {
    let (directory, store) = setup();
    let original = store.save(None, b"original").unwrap();
    replace_payload_without_advancing_nonce(directory.path(), b"externally changed");
    assert_eq!(
        failure(store.save(original.generation.as_deref(), b"lost external edit")).code,
        ErrorCode::Conflict
    );
    let observed = store.load().unwrap();
    assert_eq!(
        observed.payload.as_deref(),
        Some(b"externally changed".as_slice())
    );
    assert_ne!(observed.generation, original.generation);
    assert!(
        store
            .save(observed.generation.as_deref(), b"accepted new edit")
            .is_ok()
    );
}

#[test]
fn reused_nonce_does_not_back_up_bytes_the_host_has_not_validated() {
    let (directory, store) = setup();
    let original = store.save(None, b"validated original").unwrap();
    let backup = checkpoint(&store, original.generation.as_deref().unwrap()).unwrap();
    let path = checkpoint_path(directory.path());
    let original_bytes = fs::read(&path).unwrap();
    replace_payload_without_advancing_nonce(directory.path(), b"unvalidated changed bytes");
    assert_eq!(
        failure(checkpoint(&store, original.generation.as_deref().unwrap())).code,
        ErrorCode::Conflict
    );
    assert_eq!(fs::read(&path).unwrap(), original_bytes);
    assert_eq!(store.read_backup().unwrap().snapshot, backup.snapshot);
    let observed = store.load().unwrap();
    assert_eq!(
        checkpoint(&store, observed.generation.as_deref().unwrap())
            .unwrap()
            .snapshot,
        observed
    );
}
#[test]
fn ownership_publication_never_replaces_existing_entries() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("store-owner");
    for bytes in [b"".as_slice(), b"com.", b"foreign-owner"] {
        fs::write(&marker, bytes).unwrap();
        assert_eq!(
            failure(filesystem::publish_new(&marker, b"com.example.fixture")).code,
            ErrorCode::IoFailure
        );
        assert_eq!(fs::read(&marker).unwrap(), bytes);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(
            failure(ConfigurationStore::open(configuration(root.path()))).code,
            ErrorCode::OwnerMismatch
        );
        assert!(!root.path().join("store.lock").exists());
    }
    fs::remove_file(&marker).unwrap();
    fs::create_dir(&marker).unwrap();
    assert!(filesystem::publish_new(&marker, b"com.example.fixture").is_err());
    assert!(marker.is_dir());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}
#[test]
fn in_flight_initialization_is_busy_and_unclaimed_data_stays_unclaimed() {
    let root = tempfile::tempdir().unwrap();
    let lease = filesystem::lock(root.path()).unwrap();
    let unclaimed = root.path().join("unclaimed-data");
    fs::write(&unclaimed, b"must not be adopted or deleted").unwrap();
    assert_eq!(
        failure(ConfigurationStore::open(configuration(root.path()))).code,
        ErrorCode::Busy
    );
    drop(lease);
    assert_eq!(
        failure(ConfigurationStore::open(configuration(root.path()))).code,
        ErrorCode::OwnerMismatch
    );
    assert_eq!(
        fs::read(&unclaimed).unwrap(),
        b"must not be adopted or deleted"
    );
    assert!(!root.path().join("store-owner").exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}
#[cfg(unix)]
#[test]
fn ownership_publication_does_not_follow_an_existing_link() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("store-owner");
    let target = root.path().join("original");
    fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, &marker).unwrap();
    assert!(filesystem::publish_new(&marker, b"com.example.fixture").is_err());
    assert_eq!(fs::read_link(&marker).unwrap(), target);
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}
#[cfg(unix)]
#[test]
fn ownership_publication_preserves_complete_marker_after_directory_sync_failure() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("store-owner");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o300)).unwrap();
    let result = filesystem::publish_new(&marker, b"com.example.fixture");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(failure(result).code, ErrorCode::IoFailure);
    assert_eq!(fs::read(&marker).unwrap(), b"com.example.fixture");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    let store = ConfigurationStore::open(configuration(root.path())).unwrap();
    assert_eq!(store.load().unwrap().generation, None);
}
#[cfg(windows)]
#[test]
fn published_entries_do_not_retain_temporary_file_attributes() {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_TEMPORARY;
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("entry");
    filesystem::publish_new(&marker, b"initial").unwrap();
    assert_eq!(
        fs::metadata(&marker).unwrap().file_attributes() & FILE_ATTRIBUTE_TEMPORARY,
        0
    );
    let stage = filesystem::staged_file(&marker, b"updated")
        .unwrap()
        .into_temp_path();
    filesystem::atomic_replace(&stage, &marker).unwrap();
    assert_eq!(
        fs::metadata(&marker).unwrap().file_attributes() & FILE_ATTRIBUTE_TEMPORARY,
        0
    );
    assert_eq!(fs::read(&marker).unwrap(), b"updated");
}
#[test]
fn roundtrip_opaque_bytes_and_empty_payload_are_distinct_from_uninitialized() {
    let (dir, store) = setup();
    assert_eq!(
        store.load().unwrap(),
        Snapshot {
            generation: None,
            payload: None
        }
    );
    let data = b"\x00\xff{\"not-a-schema\":9007199254740993}";
    let first = store.save(None, data).unwrap();
    assert_eq!(store.load().unwrap(), first);
    let reopened = ConfigurationStore::open(configuration(dir.path())).unwrap();
    assert_eq!(reopened.load().unwrap(), first);
    assert_eq!(
        store
            .save(first.generation.as_deref(), data)
            .unwrap()
            .generation,
        first.generation
    );
    let empty = store.save(first.generation.as_deref(), b"").unwrap();
    assert_eq!(empty.payload, Some(vec![]));
    assert!(empty.generation.is_some());
    let wire = serde_json::to_vec(&empty).unwrap();
    assert_eq!(serde_json::from_slice::<Snapshot>(&wire).unwrap(), empty);
}
#[test]
fn stale_writes_do_not_become_authorized_by_observation() {
    let (dir, one) = setup();
    let two = ConfigurationStore::open(configuration(dir.path())).unwrap();
    let a = one.save(None, b"a").unwrap();
    let b = two.save(a.generation.as_deref(), b"b").unwrap();
    assert_eq!(one.load().unwrap(), b);
    assert_eq!(
        failure(one.save(a.generation.as_deref(), b"lost")).code,
        ErrorCode::Conflict
    );
    assert_eq!(one.load().unwrap(), b);
}

#[cfg(unix)]
#[test]
fn failed_initial_publication_can_retry_without_repairing_the_store() {
    use std::os::unix::fs::PermissionsExt;

    for reopen in [false, true] {
        let (dir, store) = setup();
        filesystem::directory(&dir.path().join("Generations")).unwrap();
        let permissions = fs::metadata(dir.path()).unwrap().permissions();
        // Permit the database write but reject publishing its marker in the parent.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = store.save(None, b"not acknowledged");
        fs::set_permissions(dir.path(), permissions).unwrap();

        assert_eq!(failure(result).code, ErrorCode::IoFailure);
        assert!(!dir.path().join("active-generation").exists());
        let retrying = if reopen {
            ConfigurationStore::open(configuration(dir.path())).unwrap()
        } else {
            store
        };
        assert_eq!(retrying.load().unwrap().generation, None);
        let accepted = retrying.save(None, b"retry").unwrap();
        assert_eq!(retrying.load().unwrap(), accepted);
        assert_eq!(accepted.payload.as_deref(), Some(b"retry".as_slice()));
    }
}

#[cfg(unix)]
#[test]
fn failed_initial_acknowledgment_preserves_the_published_generation() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, store) = setup();
    filesystem::directory(&dir.path().join("Generations")).unwrap();
    let permissions = fs::metadata(dir.path()).unwrap().permissions();
    // Creation and rename succeed; opening the parent for its durability sync fails.
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o300)).unwrap();
    let result = store.save(None, b"published before sync failure");
    fs::set_permissions(dir.path(), permissions).unwrap();

    assert_eq!(failure(result).code, ErrorCode::IoFailure);
    assert!(selected(dir.path()).is_file());
    let published = store.load().unwrap();
    assert_eq!(
        published.payload.as_deref(),
        Some(b"published before sync failure".as_slice())
    );
    let reopened = ConfigurationStore::open(configuration(dir.path())).unwrap();
    assert_eq!(reopened.load().unwrap(), published);
    assert_eq!(
        failure(store.save(None, b"must not replace")).code,
        ErrorCode::Conflict
    );
    assert_eq!(
        failure(reopened.save(None, b"must not replace")).code,
        ErrorCode::Conflict
    );
    assert_eq!(reopened.load().unwrap(), published);
}
#[test]
fn backup_reuse_tokens_and_non_destructive_restoration() {
    let (dir, store) = setup();
    let a = store.save(None, b"original").unwrap();
    let backup = checkpoint(&store, a.generation.as_deref().unwrap()).unwrap();
    assert_eq!(
        checkpoint(&store, a.generation.as_deref().unwrap()).unwrap(),
        backup
    );
    let b = store.save(a.generation.as_deref(), b"edited").unwrap();
    let original = selected(dir.path());
    let bytes = fs::read(&original).unwrap();
    assert_eq!(
        failure(store.restore_backup(&backup.token)).code,
        ErrorCode::BackupChanged
    );
    let validated = store.read_backup().unwrap();
    let restored = store.restore_backup(&validated.token).unwrap();
    assert_eq!(restored.payload, a.payload);
    assert_ne!(restored.generation, a.generation);
    assert_eq!(fs::read(&original).unwrap(), bytes);
    assert_ne!(selected(dir.path()), original);
    assert_eq!(
        failure(store.save(b.generation.as_deref(), b"stale")).code,
        ErrorCode::Conflict
    );
}

#[test]
fn corrupt_backup_failures_capture_the_backup_not_the_healthy_primary() {
    let (dir, store) = setup();
    let saved = store.save(None, b"healthy primary").unwrap();
    checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
    let path = checkpoint_path(dir.path());
    fs::write(&path, b"owned corrupt checkpoint").unwrap();
    let error = failure(checkpoint(&store, saved.generation.as_deref().unwrap()));
    assert_eq!(error.code, ErrorCode::InvalidStoredData);
    let captured = error.recovery_directory.unwrap();
    assert_eq!(
        fs::read(captured.join(path.file_name().unwrap())).unwrap(),
        b"owned corrupt checkpoint"
    );
    assert_eq!(fs::read_dir(captured).unwrap().count(), 1);
    assert_eq!(store.load().unwrap(), saved);
}

#[test]
fn repeated_raw_capture_reuses_verified_bytes_and_changed_families_stay_distinct() {
    let (dir, store) = setup();
    let saved = store.save(None, b"checkpoint").unwrap();
    checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
    let path = checkpoint_path(dir.path());
    let journal = filesystem::family_member(&path, "-journal");
    fs::write(&journal, b"raw rollback journal").unwrap();
    let first = failure(store.read_backup()).recovery_directory.unwrap();
    for _ in 0..3 {
        assert_eq!(
            failure(store.read_backup()).recovery_directory.as_ref(),
            Some(&first)
        );
    }
    assert_eq!(
        fs::read_dir(dir.path().join("Recovery")).unwrap().count(),
        1
    );
    fs::write(&journal, b"different rollback bytes").unwrap();
    let second = failure(store.read_backup()).recovery_directory.unwrap();
    assert_ne!(first, second);
    assert_eq!(
        fs::read(first.join(journal.file_name().unwrap())).unwrap(),
        b"raw rollback journal"
    );
    assert_eq!(
        fs::read(second.join(journal.file_name().unwrap())).unwrap(),
        b"different rollback bytes"
    );
    assert_eq!(store.load().unwrap(), saved);
}

#[cfg(unix)]
#[test]
fn verified_raw_capture_can_be_reused_when_recovery_directory_is_read_only() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, store) = setup();
    let saved = store.save(None, b"checkpoint").unwrap();
    checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
    let path = checkpoint_path(dir.path());
    fs::write(
        filesystem::family_member(&path, "-journal"),
        b"retained journal",
    )
    .unwrap();
    let first = failure(store.read_backup()).recovery_directory.unwrap();
    let recovery = dir.path().join("Recovery");
    let original_permissions = fs::metadata(&recovery).unwrap().permissions();
    fs::set_permissions(&recovery, fs::Permissions::from_mode(0o500)).unwrap();
    let result = store.read_backup();
    fs::set_permissions(&recovery, original_permissions).unwrap();
    let error = failure(result);
    assert_eq!(error.code, ErrorCode::InvalidStoredData);
    assert_eq!(error.recovery_directory.as_ref(), Some(&first));
    assert_eq!(fs::read_dir(recovery).unwrap().count(), 1);
}

#[test]
fn modified_retained_capture_is_never_trusted_or_overwritten() {
    let (dir, store) = setup();
    let saved = store.save(None, b"checkpoint").unwrap();
    checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
    let path = checkpoint_path(dir.path());
    let journal = filesystem::family_member(&path, "-journal");
    fs::write(&journal, b"source journal").unwrap();
    let first = failure(store.read_backup()).recovery_directory.unwrap();
    let retained = first.join(journal.file_name().unwrap());
    fs::write(&retained, b"altered retained copy").unwrap();
    let error = failure(store.read_backup());
    assert_eq!(error.code, ErrorCode::InvalidStoredData);
    assert!(error.recovery_directory.is_none());
    assert_eq!(fs::read(&retained).unwrap(), b"altered retained copy");
    assert_eq!(fs::read(&journal).unwrap(), b"source journal");
    assert_eq!(
        fs::read_dir(dir.path().join("Recovery")).unwrap().count(),
        1
    );
}

#[test]
fn reconstruction_retains_corrupt_backup_family_and_selects_verified_current_bytes() {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let (dir, store) = setup();
        let saved = store.save(None, b"old").unwrap();
        checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
        let original = checkpoint_path(dir.path());
        let member = filesystem::family_member(&original, suffix);
        fs::write(&member, b"raw retained backup bytes").unwrap();
        let old_main = fs::read(&original).unwrap();
        let current = store
            .save(saved.generation.as_deref(), b"validated latest")
            .unwrap();
        let generation = current.generation.as_deref().unwrap();
        let token = store.prepare_backup_rebuild(generation).unwrap();
        let rebuilt = store.rebuild_backup(generation, &token).unwrap();
        assert_eq!(rebuilt.snapshot, current);
        assert_ne!(checkpoint_path(dir.path()), original);
        assert_eq!(fs::read(&member).unwrap(), b"raw retained backup bytes");
        assert_eq!(fs::read(original).unwrap(), old_main);
        assert_eq!(store.read_backup().unwrap(), rebuilt);
        assert_eq!(store.load().unwrap(), current);
        assert_eq!(
            failure(store.rebuild_backup(generation, &token)).code,
            ErrorCode::BackupChanged
        );
    }
}

#[test]
fn reconstruction_keeps_a_complete_damaged_checkpoint_through_review() {
    let (dir, store) = setup();
    let first = store.save(None, &[0, 255, 1]).unwrap();
    checkpoint(&store, first.generation.as_deref().unwrap()).unwrap();
    let original = checkpoint_path(dir.path());
    let current = store
        .save(first.generation.as_deref(), &[128, 0, 254])
        .unwrap();
    let generation = current.generation.as_deref().unwrap();
    let primary = selected(dir.path());
    let primary_bytes = fs::read(&primary).unwrap();
    let mut family = ["", "-journal", "-wal", "-shm"].map(|suffix| {
        let path = filesystem::family_member(&original, suffix);
        let bytes = format!("damaged backup member {suffix}").into_bytes();
        fs::write(&path, &bytes).unwrap();
        (path, bytes)
    });

    let refusal = failure(store.prepare_backup(generation));
    assert_eq!(refusal.code, ErrorCode::InvalidStoredData);
    let capture = refusal.recovery_directory.unwrap();
    for (path, bytes) in &family {
        assert_eq!(
            fs::read(capture.join(path.file_name().unwrap())).unwrap(),
            *bytes
        );
    }
    assert!(!capture.join(primary.file_name().unwrap()).exists());

    let reviewed = store.prepare_backup_rebuild(generation).unwrap();
    assert_eq!(checkpoint_path(dir.path()), original);
    family[0].1 = b"changed backup damage after review".to_vec();
    fs::write(&family[0].0, &family[0].1).unwrap();
    assert_eq!(
        failure(store.rebuild_backup(generation, &reviewed)).code,
        ErrorCode::BackupChanged
    );
    assert_eq!(checkpoint_path(dir.path()), original);
    for (path, bytes) in &family {
        assert_eq!(fs::read(path).unwrap(), *bytes);
    }

    let token = store.prepare_backup_rebuild(generation).unwrap();
    assert!(!token.is_empty());
    assert_ne!(token, reviewed);
    assert_eq!(checkpoint_path(dir.path()), original);
    for (path, bytes) in &family {
        assert_eq!(fs::read(path).unwrap(), *bytes);
    }

    let rebuilt = store.rebuild_backup(generation, &token).unwrap();
    assert_eq!(rebuilt.snapshot, current);
    assert_ne!(checkpoint_path(dir.path()), original);
    let reopened = ConfigurationStore::open_existing(configuration(dir.path())).unwrap();
    assert_eq!(reopened.read_backup().unwrap(), rebuilt);
    for (path, bytes) in family {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(selected(dir.path()), primary);
    assert_eq!(fs::read(primary).unwrap(), primary_bytes);
    assert_eq!(store.load().unwrap(), current);
    assert_eq!(
        failure(store.rebuild_backup(generation, &token)).code,
        ErrorCode::BackupChanged
    );
}

#[test]
fn reconstruction_rejects_changed_primary_backup_or_selector_without_publication() {
    for change in ["primary", "backup", "selector"] {
        let (dir, store) = setup();
        let saved = store.save(None, b"validated").unwrap();
        let generation = saved.generation.as_deref().unwrap();
        checkpoint(&store, generation).unwrap();
        let original = checkpoint_path(dir.path());
        let token = store.prepare_backup_rebuild(generation).unwrap();
        let expected = match change {
            "primary" => {
                store.save(Some(generation), b"external edit").unwrap();
                ErrorCode::BackupChanged
            }
            "backup" => {
                fs::write(&original, b"changed after review").unwrap();
                ErrorCode::BackupChanged
            }
            _ => {
                fs::write(dir.path().join("active-backup"), b"changed marker").unwrap();
                ErrorCode::BackupChanged
            }
        };
        let selector = fs::read(dir.path().join("active-backup")).unwrap();
        let old_bytes = fs::read(&original).unwrap();
        assert_eq!(
            failure(store.rebuild_backup(generation, &token)).code,
            expected
        );
        assert_eq!(fs::read(&original).unwrap(), old_bytes);
        assert_eq!(
            fs::read(dir.path().join("active-backup")).unwrap(),
            selector
        );
        assert_eq!(fs::read_dir(dir.path().join("Backups")).unwrap().count(), 1);
    }
}

#[test]
fn a_second_writer_expires_review_even_with_the_new_primary_generation() {
    let (dir, store) = setup();
    let first = store.save(None, b"reviewed").unwrap();
    let generation = first.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let backup = checkpoint_path(dir.path());
    let backup_bytes = fs::read(&backup).unwrap();
    let review = store.prepare_backup_rebuild(generation).unwrap();

    let other = ConfigurationStore::open_existing(configuration(dir.path())).unwrap();
    let newer = other.save(Some(generation), b"latest").unwrap();
    assert_eq!(
        failure(store.prepare_backup_rebuild(generation)).code,
        ErrorCode::Conflict
    );
    for expected in [generation, newer.generation.as_deref().unwrap()] {
        assert_eq!(
            failure(store.rebuild_backup(expected, &review)).code,
            ErrorCode::BackupChanged
        );
    }
    assert_eq!(checkpoint_path(dir.path()), backup);
    assert_eq!(fs::read(backup).unwrap(), backup_bytes);
    assert_eq!(store.load().unwrap(), newer);
}

#[test]
fn unreviewed_primary_sidecars_expire_backup_rebuild_review_without_consuming_evidence() {
    for suffix in ["-journal", "-wal", "-shm"] {
        let (dir, store) = setup();
        let saved = store.save(None, b"reviewed").unwrap();
        let generation = saved.generation.as_deref().unwrap();
        checkpoint(&store, generation).unwrap();
        let backup = checkpoint_path(dir.path());
        let backup_bytes = fs::read(&backup).unwrap();
        let token = store.prepare_backup_rebuild(generation).unwrap();
        let primary = selected(dir.path());
        let member = filesystem::family_member(&primary, suffix);
        let bytes = format!("unreviewed primary sidecar {suffix}").into_bytes();
        fs::write(&member, &bytes).unwrap();

        assert_eq!(
            failure(store.rebuild_backup(generation, &token)).code,
            ErrorCode::BackupChanged,
            "{suffix}"
        );
        assert_eq!(fs::read(member).unwrap(), bytes);
        assert_eq!(selected(dir.path()), primary);
        assert_eq!(checkpoint_path(dir.path()), backup);
        assert_eq!(fs::read(backup).unwrap(), backup_bytes);
    }
}

#[test]
fn readable_foreign_and_newer_checkpoints_cannot_be_rebuilt() {
    use sha2::{Digest, Sha256};
    for (update, expected) in [
        (
            "UPDATE store_state SET owner='foreign'",
            ErrorCode::OwnerMismatch,
        ),
        ("PRAGMA user_version=2", ErrorCode::UnsupportedFormatVersion),
    ] {
        for (sidecar, legacy, desktop_metadata) in [
            ("", false, false),
            ("-journal", false, false),
            ("-wal", false, false),
            ("-wal", true, false),
            ("", true, true),
            ("-wal", true, true),
        ] {
            let (dir, store) = setup();
            let saved = store.save(None, b"valid primary").unwrap();
            let generation = saved.generation.as_deref().unwrap();
            checkpoint(&store, generation).unwrap();
            let selector = dir.path().join("active-backup");
            let mut path = checkpoint_path(dir.path());
            if legacy {
                let fixed = dir.path().join("Backups/snapshot.sqlite");
                fs::rename(path, &fixed).unwrap();
                fs::remove_file(&selector).unwrap();
                path = fixed;
            }
            if desktop_metadata {
                fs::write(dir.path().join("Backups/.DS_Store"), b"desktop metadata").unwrap();
            }
            let selected = fs::read(&selector).ok();
            let db = rusqlite::Connection::open(&path).unwrap();
            let _wal_connection = if sidecar == "-wal" {
                db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
                    .unwrap();
                let checkpointed = fs::read(&path).unwrap();
                db.execute_batch(update).unwrap();
                assert_eq!(fs::read(&path).unwrap(), checkpointed);
                assert!(
                    fs::metadata(filesystem::family_member(&path, "-wal"))
                        .unwrap()
                        .len()
                        > 32
                );
                Some(db)
            } else {
                db.execute_batch(update).unwrap();
                db.close().unwrap();
                if sidecar == "-journal" {
                    fs::write(filesystem::family_member(&path, sidecar), []).unwrap();
                }
                None
            };
            let family = ["", "-journal", "-wal", "-shm"]
                .map(|suffix| filesystem::family_member(&path, suffix));
            let retained = family.each_ref().map(|file| fs::read(file).ok());
            assert_eq!(
                failure(store.prepare_backup_rebuild(generation)).code,
                expected,
                "prepare: {sidecar}, legacy={legacy}"
            );
            // A raw token has the same format as one issued by older versions.
            // Publication must repeat the ownership and format checks.
            let mut legacy_token = Sha256::new();
            legacy_token.update(b"configuration-storage-backup-rebuild-v1\0");
            for bytes in [
                configuration(dir.path()).owner.as_bytes(),
                generation.as_bytes(),
            ] {
                legacy_token.update((bytes.len() as u64).to_le_bytes());
                legacy_token.update(bytes);
            }
            legacy_token.update(
                filesystem::selection_fingerprint(dir.path())
                    .unwrap()
                    .digest,
            );
            legacy_token.update(
                filesystem::backup_fingerprint(dir.path(), None)
                    .unwrap()
                    .digest,
            );
            let token: String = legacy_token
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(
                failure(store.rebuild_backup(generation, &token)).code,
                expected,
                "commit: {sidecar}, legacy={legacy}"
            );
            assert_eq!(family.each_ref().map(|file| fs::read(file).ok()), retained);
            assert_eq!(fs::read(&selector).ok(), selected);
            assert_eq!(store.load().unwrap(), saved);
        }
    }
}

#[test]
fn desktop_metadata_keeps_legacy_backup_review_and_restore_available() {
    let (dir, store) = setup();
    let saved = store.save(None, b"legacy checkpoint").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let directory = dir.path().join("Backups");
    let legacy = directory.join("snapshot.sqlite");
    fs::rename(checkpoint_path(dir.path()), &legacy).unwrap();
    fs::remove_file(dir.path().join("active-backup")).unwrap();
    let original = fs::read(&legacy).unwrap();
    let expected = store.read_backup().unwrap();
    let rotation = store.prepare_backup(generation).unwrap();
    let rebuild = store.prepare_backup_rebuild(generation).unwrap();

    let metadata = [".DS_Store", "Desktop.ini", "Thumbs.db"];
    for name in metadata {
        fs::write(directory.join(name), name.as_bytes()).unwrap();
    }
    assert_eq!(store.read_backup().unwrap(), expected);
    assert_eq!(store.prepare_backup(generation).unwrap(), rotation);
    assert_eq!(store.prepare_backup_rebuild(generation).unwrap(), rebuild);
    assert!(
        store
            .list_snapshot_candidates(64, None, Some(crate::SnapshotCandidateSource::Backup))
            .unwrap()
            .candidates
            .iter()
            .any(|candidate| candidate.id == "backup:snapshot"
                && candidate.is_selected == Some(true))
    );
    assert_eq!(
        store.create_backup(generation, &rotation.token).unwrap(),
        expected
    );
    let restored = store.restore_backup(&expected.token).unwrap();
    assert_eq!(restored.payload, saved.payload);
    assert_ne!(restored.generation, saved.generation);
    assert_eq!(fs::read(legacy).unwrap(), original);
    for name in metadata {
        assert_eq!(fs::read(directory.join(name)).unwrap(), name.as_bytes());
    }
}

#[test]
fn desktop_metadata_does_not_disambiguate_an_orphaned_versioned_family() {
    let (dir, store) = setup();
    let saved = store.save(None, b"legacy checkpoint").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let path = checkpoint_path(dir.path());
    let directory = path.parent().unwrap();
    let legacy = directory.join("snapshot.sqlite");
    fs::rename(&path, &legacy).unwrap();
    fs::remove_file(dir.path().join("active-backup")).unwrap();
    fs::write(directory.join("desktop.ini"), b"desktop metadata").unwrap();
    let journal = filesystem::family_member(&path, "-journal");
    fs::write(&journal, b"orphaned journal bytes").unwrap();

    assert_eq!(
        failure(store.read_backup()).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(
        failure(store.prepare_backup(generation)).code,
        ErrorCode::InvalidStoredData
    );
    let retained = store.read_snapshot_candidate("backup:snapshot").unwrap();
    assert_eq!(retained.is_selected, None);
    assert_eq!(retained.snapshot, saved);
    assert_eq!(fs::read(journal).unwrap(), b"orphaned journal bytes");
}

#[test]
fn legacy_fixed_checkpoint_remains_readable_and_is_retained_during_reconstruction() {
    let (dir, store) = setup();
    let saved = store.save(None, b"legacy checkpoint").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let versioned = checkpoint_path(dir.path());
    let legacy = dir.path().join("Backups/snapshot.sqlite");
    fs::rename(versioned, &legacy).unwrap();
    fs::remove_file(dir.path().join("active-backup")).unwrap();
    let original = fs::read(&legacy).unwrap();
    assert_eq!(store.read_backup().unwrap().snapshot, saved);
    fs::write(&legacy, b"damaged old layout").unwrap();
    let token = store.prepare_backup_rebuild(generation).unwrap();
    assert_eq!(
        store.rebuild_backup(generation, &token).unwrap().snapshot,
        saved
    );
    assert_eq!(fs::read(&legacy).unwrap(), b"damaged old layout");
    assert_ne!(
        fs::read(checkpoint_path(dir.path())).unwrap(),
        b"damaged old layout"
    );
    assert!(!original.is_empty());
}

#[test]
fn ordinary_checkpoint_rotation_does_not_accumulate_healthy_copies() {
    let (dir, store) = setup();
    let first = store.save(None, b"first").unwrap();
    checkpoint(&store, first.generation.as_deref().unwrap()).unwrap();
    let path = checkpoint_path(dir.path());
    let mut current = first;
    for payload in [b"second".as_slice(), b"third", b"fourth"] {
        current = store.save(current.generation.as_deref(), payload).unwrap();
        let checkpoint = checkpoint(&store, current.generation.as_deref().unwrap()).unwrap();
        assert_eq!(checkpoint.snapshot, current);
        assert_eq!(checkpoint_path(dir.path()), path);
        assert_eq!(fs::read_dir(dir.path().join("Backups")).unwrap().count(), 1);
    }
}

#[test]
fn invalid_checkpoint_column_types_are_reconstructible_data_failures() {
    for value in ["42", "'not an opaque blob'"] {
        let (dir, store) = setup();
        let saved = store.save(None, b"validated primary").unwrap();
        let generation = saved.generation.as_deref().unwrap();
        checkpoint(&store, generation).unwrap();
        let old = checkpoint_path(dir.path());
        let connection = rusqlite::Connection::open(&old).unwrap();
        connection
            .execute_batch(&format!("UPDATE store_state SET payload={value}"))
            .unwrap();
        connection.close().unwrap();
        let bytes = fs::read(&old).unwrap();
        assert_eq!(
            failure(store.read_backup()).code,
            ErrorCode::InvalidStoredData
        );
        let token = store.prepare_backup_rebuild(generation).unwrap();
        assert_eq!(
            store.rebuild_backup(generation, &token).unwrap().snapshot,
            saved
        );
        assert_eq!(fs::read(old).unwrap(), bytes);
    }
}

#[test]
fn malformed_checkpoint_selector_is_captured_before_reconstruction() {
    let (dir, store) = setup();
    let saved = store.save(None, b"verified primary").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let old = checkpoint_path(dir.path());
    let bytes = fs::read(&old).unwrap();
    let marker = dir.path().join("active-backup");
    fs::write(&marker, b"damaged backup selector").unwrap();
    let error = failure(store.read_backup());
    assert_eq!(error.code, ErrorCode::InvalidStoredData);
    let capture = error.recovery_directory.unwrap();
    assert_eq!(
        fs::read(capture.join("active-backup")).unwrap(),
        b"damaged backup selector"
    );
    let token = store.prepare_backup_rebuild(generation).unwrap();
    assert_eq!(
        store.rebuild_backup(generation, &token).unwrap().snapshot,
        saved
    );
    assert_eq!(fs::read(old).unwrap(), bytes);
    assert_eq!(
        fs::read(capture.join("active-backup")).unwrap(),
        b"damaged backup selector"
    );
}

#[test]
fn retained_checkpoint_selector_alone_never_authorizes_reseeding() {
    let (dir, store) = setup();
    let marker = dir.path().join("active-backup");
    let bytes = uuid::Uuid::new_v4().to_string();
    fs::write(&marker, &bytes).unwrap();
    assert_eq!(failure(store.load()).code, ErrorCode::InvalidStoredData);
    assert_eq!(
        failure(store.save(None, b"must not seed")).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), bytes);
    assert!(!dir.path().join("Generations").exists());
}

#[test]
fn missing_selection_with_retained_backup_requires_explicit_restore() {
    for keep_empty_generations in [false, true] {
        let (dir, store) = setup();
        let saved = store.save(None, b"backed up data").unwrap();
        checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
        let backup_path = checkpoint_path(dir.path());
        let backup_bytes = fs::read(&backup_path).unwrap();
        fs::remove_file(dir.path().join("active-generation")).unwrap();
        fs::remove_dir_all(dir.path().join("Generations")).unwrap();
        if keep_empty_generations {
            filesystem::directory(&dir.path().join("Generations")).unwrap();
        }
        let reopened = ConfigurationStore::open(configuration(dir.path())).unwrap();
        let loaded = reopened.load();
        let seeded = reopened.save(None, b"must not seed");
        assert!(loaded.is_err(), "load={loaded:?}, seed={seeded:?}");
        assert_eq!(failure(loaded).code, ErrorCode::InvalidStoredData);
        assert_eq!(failure(seeded).code, ErrorCode::InvalidStoredData);
        assert_eq!(fs::read(&backup_path).unwrap(), backup_bytes);

        let backup = reopened.read_backup().unwrap();
        assert_eq!(backup.snapshot.payload, saved.payload);
        let restored = reopened.restore_backup(&backup.token).unwrap();
        assert_eq!(restored.payload, saved.payload);
        assert_ne!(restored.generation, saved.generation);
        assert_eq!(reopened.load().unwrap(), restored);
        assert_eq!(fs::read(&backup_path).unwrap(), backup_bytes);
    }
}

#[test]
fn missing_selection_with_raw_recovery_never_seeds_over_retained_evidence() {
    let (dir, store) = setup();
    store.save(None, b"preserved raw data").unwrap();
    let database = selected(dir.path());
    let bytes = fs::read(&database).unwrap();
    let capture = filesystem::preserve_family(&database, dir.path()).unwrap();
    let retained = capture.join(database.file_name().unwrap());
    fs::remove_file(dir.path().join("active-generation")).unwrap();
    fs::remove_dir_all(dir.path().join("Generations")).unwrap();

    let loaded = store.load();
    let seeded = store.save(None, b"must not seed");
    assert!(loaded.is_err(), "load={loaded:?}, seed={seeded:?}");
    assert_eq!(failure(loaded).code, ErrorCode::InvalidStoredData);
    assert_eq!(failure(seeded).code, ErrorCode::InvalidStoredData);
    assert_eq!(fs::read(retained).unwrap(), bytes);
}

#[test]
fn empty_managed_directories_do_not_prevent_first_initialization() {
    let (dir, store) = setup();
    for name in ["Generations", "Backups", "Recovery"] {
        filesystem::directory(&dir.path().join(name)).unwrap();
    }
    assert_eq!(store.load().unwrap().generation, None);
    let saved = store.save(None, b"first value").unwrap();
    assert_eq!(store.load().unwrap(), saved);
}
#[test]
fn changed_backup_is_rejected_after_host_validation() {
    let (_directory, store) = setup();
    let a = store.save(None, b"a").unwrap();
    let old = checkpoint(&store, a.generation.as_deref().unwrap()).unwrap();
    let b = store.save(a.generation.as_deref(), b"b").unwrap();
    checkpoint(&store, b.generation.as_deref().unwrap()).unwrap();
    assert_eq!(
        failure(store.restore_backup(&old.token)).code,
        ErrorCode::BackupChanged
    );
    assert_eq!(store.load().unwrap(), b);
}
#[test]
fn corrupted_selected_store_retains_raw_files_and_restores_without_replacing_them() {
    let (dir, store) = setup();
    let a = store.save(None, b"safe").unwrap();
    let backup = checkpoint(&store, a.generation.as_deref().unwrap()).unwrap();
    let old = selected(dir.path());
    assert_eq!(backup.snapshot, a);
    fs::write(&old, b"not a SQLite file").unwrap();
    fs::write(filesystem::family_member(&old, "-journal"), b"interrupted").unwrap();
    let error = failure(store.load());
    assert_eq!(error.code, ErrorCode::InvalidStoredData);
    let recovery = error.recovery_directory.unwrap();
    assert_eq!(
        fs::read(recovery.join(old.file_name().unwrap())).unwrap(),
        b"not a SQLite file"
    );
    assert_eq!(
        fs::read(
            recovery.join(
                filesystem::family_member(&old, "-journal")
                    .file_name()
                    .unwrap()
            )
        )
        .unwrap(),
        b"interrupted"
    );
    assert_eq!(
        fs::read_dir(dir.path().join("Recovery")).unwrap().count(),
        1
    );
    assert_eq!(
        store
            .restore_backup(&store.read_backup().unwrap().token)
            .unwrap()
            .payload,
        a.payload
    );
    assert_eq!(fs::read(old).unwrap(), b"not a SQLite file");
}
#[test]
fn missing_or_invalid_selected_generation_never_seeds_an_empty_store() {
    let (dir, store) = setup();
    let a = store.save(None, b"data").unwrap();
    let file = selected(dir.path());
    fs::remove_file(file).unwrap();
    assert_eq!(failure(store.load()).code, ErrorCode::InvalidStoredData);
    assert_eq!(
        failure(store.save(None, b"replacement")).code,
        ErrorCode::InvalidStoredData
    );
    fs::write(dir.path().join("active-generation"), b"../../elsewhere").unwrap();
    assert_eq!(failure(store.load()).code, ErrorCode::InvalidStoredData);
    assert!(a.generation.is_some());
}
#[test]
fn foreign_owners_and_newer_schema_are_not_modified() {
    let (dir, store) = setup();
    store.save(None, b"value").unwrap();
    let original = selected(dir.path());
    let before = fs::read(&original).unwrap();
    let mut other = configuration(dir.path());
    other.owner = "different".into();
    assert_eq!(
        failure(ConfigurationStore::open(other)).code,
        ErrorCode::OwnerMismatch
    );
    assert_eq!(fs::read(&original).unwrap(), before);
    rusqlite::Connection::open(&original)
        .unwrap()
        .execute_batch("PRAGMA user_version=2")
        .unwrap();
    let before = fs::read(&original).unwrap();
    let error = failure(store.load());
    assert_eq!(error.code, ErrorCode::UnsupportedFormatVersion);
    assert_eq!(
        serde_json::to_string(&error).unwrap(),
        r#"{"code":"unsupportedFormatVersion"}"#
    );
    assert_eq!(fs::read(original).unwrap(), before);
}
#[test]
fn payload_digest_and_exclusive_root_lock_protect_against_external_changes() {
    let (dir, store) = setup();
    store.save(None, b"value").unwrap();
    let lease = filesystem::lock(dir.path()).unwrap();
    assert_eq!(failure(store.load()).code, ErrorCode::Busy);
    drop(lease);
    rusqlite::Connection::open(selected(dir.path()))
        .unwrap()
        .execute("UPDATE store_state SET payload=?1", [b"bad".as_slice()])
        .unwrap();
    assert_eq!(failure(store.load()).code, ErrorCode::InvalidStoredData);
}
#[test]
fn invalid_limits_unowned_directories_and_missing_backups_are_conservative() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = configuration(dir.path());
    config.maximum_payload_bytes = 0;
    assert_eq!(
        failure(ConfigurationStore::open(config)).code,
        ErrorCode::InvalidConfiguration
    );
    fs::write(dir.path().join("unrelated"), b"keep").unwrap();
    assert_eq!(
        failure(ConfigurationStore::open(configuration(dir.path()))).code,
        ErrorCode::OwnerMismatch
    );
    let (_directory, store) = setup();
    assert_eq!(failure(store.read_backup()).code, ErrorCode::BackupMissing);
    assert_eq!(
        failure(store.save(None, &vec![0; 1025])).code,
        ErrorCode::InvalidConfiguration
    );
    assert_eq!(store.load().unwrap().payload, None);
}
#[cfg(unix)]
#[test]
fn parent_alias_can_create_a_private_nested_root_save_and_reopen() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let fixture = tempfile::tempdir().unwrap();
    let concrete = fixture.path().join("concrete");
    fs::create_dir(&concrete).unwrap();
    let alias = fixture.path().join("alias");
    symlink(&concrete, &alias).unwrap();
    let supplied = alias.join("new-parent/ConfigurationStorage");
    let resolved = fs::canonicalize(&concrete)
        .unwrap()
        .join("new-parent/ConfigurationStorage");
    let store = ConfigurationStore::open(configuration(&supplied)).unwrap();
    assert_eq!(store.configuration().directory, resolved);
    assert_eq!(store.load().unwrap().payload, None);
    for directory in [&resolved, resolved.parent().unwrap()] {
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let saved = store.save(None, b"\x00\xffopaque").unwrap();
    for root in [&supplied, &resolved] {
        let reopened = ConfigurationStore::open(configuration(root)).unwrap();
        assert_eq!(reopened.configuration().directory, resolved);
        assert_eq!(reopened.load().unwrap(), saved);
    }
    let mut foreign = configuration(&supplied);
    foreign.owner = "another.owner".into();
    assert_eq!(
        failure(ConfigurationStore::open(foreign)).code,
        ErrorCode::OwnerMismatch
    );
    assert_eq!(store.load().unwrap(), saved);
}

#[cfg(unix)]
#[test]
fn retargeting_a_parent_alias_cannot_redirect_an_opened_store() {
    use std::os::unix::fs::symlink;

    let fixture = tempfile::tempdir().unwrap();
    let original = fixture.path().join("original");
    let other = fixture.path().join("other");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&other).unwrap();
    let alias = fixture.path().join("alias");
    symlink(&original, &alias).unwrap();
    let suffix = Path::new("new-parent/ConfigurationStorage");
    let store = ConfigurationStore::open(configuration(&alias.join(suffix))).unwrap();
    let first = store.save(None, b"original").unwrap();
    let other_store = ConfigurationStore::open(configuration(&other.join(suffix))).unwrap();
    let other_saved = other_store.save(None, b"other").unwrap();

    fs::remove_file(&alias).unwrap();
    symlink(&other, &alias).unwrap();
    assert_eq!(store.load().unwrap(), first);
    let updated = store
        .save(first.generation.as_deref(), b"still original")
        .unwrap();
    let direct = ConfigurationStore::open(configuration(&original.join(suffix))).unwrap();
    assert_eq!(direct.load().unwrap(), updated);
    assert_eq!(other_store.load().unwrap(), other_saved);
    let reopened = ConfigurationStore::open(configuration(&alias.join(suffix))).unwrap();
    assert_eq!(reopened.load().unwrap(), other_saved);
}

#[cfg(unix)]
#[test]
fn a_root_alias_is_rejected_even_with_trailing_separators_or_dot() {
    use std::os::unix::fs::symlink;

    let fixture = tempfile::tempdir().unwrap();
    let concrete = fixture.path().join("concrete");
    fs::create_dir(&concrete).unwrap();
    let alias = fixture.path().join("alias");
    symlink(&concrete, &alias).unwrap();
    for root in [alias.clone(), alias.join(""), alias.join(".")] {
        assert_eq!(
            failure(ConfigurationStore::open(configuration(&root))).code,
            ErrorCode::InvalidStoredData
        );
    }
    assert_eq!(fs::read_dir(&concrete).unwrap().count(), 0);
    let dangling = fixture.path().join("dangling");
    symlink(fixture.path().join("absent"), &dangling).unwrap();
    assert_eq!(
        failure(ConfigurationStore::open(configuration(&dangling))).code,
        ErrorCode::InvalidStoredData
    );
    assert!(!fixture.path().join("absent").exists());
}

#[test]
fn root_resolution_rejects_traversal_without_creating_missing_components() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("missing/../store");
    assert_eq!(
        failure(ConfigurationStore::open(configuration(&root))).code,
        ErrorCode::InvalidConfiguration
    );
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn generated_subdirectories_cannot_redirect_storage_outside_its_root() {
    let (dir, store) = setup();
    store.save(None, b"inside").unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let old = dir.path().join("Generations");
    let renamed = dir.path().join("retained");
    fs::rename(&old, &renamed).unwrap();
    std::os::unix::fs::symlink(foreign.path(), &old).unwrap();
    assert_eq!(failure(store.load()).code, ErrorCode::InvalidStoredData);
    assert_eq!(fs::read_dir(foreign.path()).unwrap().count(), 0);
}

#[test]
fn valid_utf16_sqlite_envelope_can_be_read() {
    let (dir, store) = setup();
    let expected = store.save(None, b"original").unwrap();
    let path = selected(dir.path());
    let original = rusqlite::Connection::open(&path).unwrap();
    let schema: String = original
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='store_state'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let (owner, nonce, payload, digest): (String, String, Vec<u8>, String) = original
        .query_row(
            "SELECT owner,generation,payload,digest FROM store_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    original.close().unwrap();
    let imported = dir.path().join("imported.sqlite");
    let connection = rusqlite::Connection::open(&imported).unwrap();
    connection
        .execute_batch(
            "PRAGMA encoding='UTF-16le'; CREATE TABLE seed (id INTEGER); DROP TABLE seed;",
        )
        .unwrap();
    let encoding: String = connection
        .query_row("PRAGMA encoding", [], |row| row.get(0))
        .unwrap();
    assert_eq!(encoding, "UTF-16le");
    connection.execute_batch(&schema).unwrap();
    connection.execute_batch("PRAGMA user_version=1;").unwrap();
    connection
        .execute(
            "INSERT INTO store_state VALUES(1,?1,?2,?3,?4)",
            rusqlite::params![owner, nonce, payload, digest],
        )
        .unwrap();
    connection.close().unwrap();
    let bytes = fs::read(imported).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert_eq!(store.load().unwrap(), expected);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(!dir.path().join("Recovery").exists());
}

#[test]
fn unexpected_sqlite_schema_cannot_acknowledge_a_write() {
    for sql in [
        "CREATE TRIGGER discard BEFORE UPDATE ON store_state BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER rewrite AFTER UPDATE ON store_state BEGIN UPDATE store_state SET generation='00000000-0000-0000-0000-000000000000'; END",
        "CREATE VIEW exposed AS SELECT payload FROM store_state",
        "CREATE INDEX extra ON store_state(owner)",
        "ALTER TABLE store_state ADD COLUMN extra TEXT",
    ] {
        let (dir, store) = setup();
        let saved = store.save(None, b"retain").unwrap();
        let path = selected(dir.path());
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(
            failure(store.load()).code,
            ErrorCode::InvalidStoredData,
            "{sql}"
        );
        let error = failure(store.save(saved.generation.as_deref(), b"must not be acknowledged"));
        assert_eq!(error.code, ErrorCode::InvalidStoredData, "{sql}");
        assert!(error.recovery_directory.is_some());
        assert_eq!(fs::read(path).unwrap(), before, "{sql}");
    }
}

#[test]
fn unexpected_backup_trigger_cannot_change_restored_payload_or_generation() {
    let (dir, store) = setup();
    let saved = store.save(None, b"retain").unwrap();
    let backup = checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
    let path = checkpoint_path(dir.path());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER discard BEFORE UPDATE ON store_state BEGIN SELECT RAISE(IGNORE); END",
        )
        .unwrap();
    let before = fs::read(&path).unwrap();
    let selected_before = selected(dir.path());
    assert_eq!(
        failure(store.read_backup()).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(
        failure(store.restore_backup(&backup.token)).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(selected(dir.path()), selected_before);
    assert_eq!(store.load().unwrap(), saved);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn accepted_writes_require_one_row_and_the_actual_expected_snapshot() {
    let (dir, store) = setup();
    let saved = store.save(None, b"retain").unwrap();
    let path = selected(dir.path());
    let mut connection = database::open(&path, true).unwrap();
    let transaction = connection.transaction().unwrap();
    let changed = transaction
        .execute("UPDATE store_state SET payload=payload WHERE id=2", [])
        .unwrap();
    assert_eq!(
        failure(database::accepted_write(
            &transaction,
            &configuration(dir.path()),
            &saved,
            changed
        ))
        .code,
        ErrorCode::InvalidStoredData
    );
    let expected = Snapshot {
        generation: saved.generation.clone(),
        payload: Some(b"different".to_vec()),
    };
    let changed = transaction
        .execute("UPDATE store_state SET payload=payload WHERE id=1", [])
        .unwrap();
    assert_eq!(
        failure(database::accepted_write(
            &transaction,
            &configuration(dir.path()),
            &expected,
            changed
        ))
        .code,
        ErrorCode::InvalidStoredData
    );
    transaction.rollback().unwrap();
    assert_eq!(store.load().unwrap(), saved);
}

#[test]
fn restore_token_rejects_a_newer_save_without_replacing_the_backup() {
    let (dir, store) = setup();
    let other = ConfigurationStore::open(configuration(dir.path())).unwrap();
    let original = store.save(None, b"original").unwrap();
    let validated = checkpoint(&store, original.generation.as_deref().unwrap()).unwrap();
    let backup_path = checkpoint_path(dir.path());
    let backup_bytes = fs::read(&backup_path).unwrap();
    let newer = other
        .save(original.generation.as_deref(), b"newer")
        .unwrap();
    let selected_before = selected(dir.path());
    assert_eq!(
        failure(store.restore_backup(&validated.token)).code,
        ErrorCode::BackupChanged
    );
    assert_eq!(selected(dir.path()), selected_before);
    assert_eq!(fs::read(backup_path).unwrap(), backup_bytes);
    assert_eq!(store.load().unwrap(), newer);
    let refreshed = store.read_backup().unwrap();
    assert_eq!(refreshed.snapshot, validated.snapshot);
    assert_ne!(refreshed.token, validated.token);
    assert_eq!(store.read_backup().unwrap(), refreshed);
    assert_eq!(
        other.save(newer.generation.as_deref(), b"newer").unwrap(),
        newer
    );
    assert_eq!(store.read_backup().unwrap().token, refreshed.token);
    assert_eq!(
        store.restore_backup(&refreshed.token).unwrap().payload,
        original.payload
    );
}

#[test]
fn recovery_tokens_bind_raw_damage_absence_and_sidecars_without_opening_selected_sqlite() {
    for change in [
        "corrupt",
        "missing",
        "missing-directory",
        "-journal",
        "-wal",
        "-shm",
        "marker-corrupt",
        "marker-missing",
    ] {
        let (dir, store) = setup();
        let saved = store.save(None, b"recover").unwrap();
        let validated = checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
        let old = selected(dir.path());
        match change {
            "corrupt" => fs::write(&old, b"corrupt database bytes").unwrap(),
            "missing" => fs::remove_file(&old).unwrap(),
            "missing-directory" => fs::rename(
                dir.path().join("Generations"),
                dir.path().join("retained-generations"),
            )
            .unwrap(),
            "marker-corrupt" => {
                fs::write(dir.path().join("active-generation"), b"../../outside").unwrap()
            }
            "marker-missing" => fs::remove_file(dir.path().join("active-generation")).unwrap(),
            suffix => fs::write(
                filesystem::family_member(&old, suffix),
                b"raw interrupted evidence",
            )
            .unwrap(),
        }
        assert_eq!(
            failure(store.restore_backup(&validated.token)).code,
            ErrorCode::BackupChanged,
            "{change}"
        );
        let refreshed = store.read_backup().unwrap();
        assert_eq!(refreshed.snapshot, saved);
        assert_ne!(refreshed.token, validated.token, "{change}");
        let restored = store.restore_backup(&refreshed.token).unwrap();
        assert_eq!(restored.payload, saved.payload, "{change}");
        assert_ne!(restored.generation, saved.generation, "{change}");
        assert_eq!(store.load().unwrap(), restored);
        match change {
            "corrupt" => assert_eq!(fs::read(&old).unwrap(), b"corrupt database bytes"),
            "missing" => assert!(!old.exists()),
            "missing-directory" => assert!(
                dir.path()
                    .join("retained-generations")
                    .join(old.file_name().unwrap())
                    .is_file()
            ),
            "marker-corrupt" | "marker-missing" => assert!(old.is_file()),
            suffix => assert_eq!(
                fs::read(filesystem::family_member(&old, suffix)).unwrap(),
                b"raw interrupted evidence"
            ),
        }
    }
}

#[test]
fn fingerprints_stream_all_bytes_and_distinguish_missing_from_empty_files() {
    let (dir, store) = setup();
    let saved = store.save(None, b"recover").unwrap();
    checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
    let path = selected(dir.path());
    let mut raw = vec![7; 256 * 1024 + 17];
    fs::write(&path, &raw).unwrap();
    let validated = store.read_backup().unwrap();
    *raw.last_mut().unwrap() = 8;
    fs::write(&path, &raw).unwrap();
    assert_eq!(
        failure(store.restore_backup(&validated.token)).code,
        ErrorCode::BackupChanged
    );
    assert_ne!(store.read_backup().unwrap().token, validated.token);
    fs::remove_file(&path).unwrap();
    let absent = store.read_backup().unwrap();
    fs::write(&path, b"").unwrap();
    let empty = store.read_backup().unwrap();
    assert_ne!(absent.token, empty.token);
    assert_eq!(
        failure(store.restore_backup(&absent.token)).code,
        ErrorCode::BackupChanged
    );
    fs::remove_file(dir.path().join("active-generation")).unwrap();
    let no_marker = store.read_backup().unwrap();
    fs::write(dir.path().join("active-generation"), b"").unwrap();
    let empty_marker = store.read_backup().unwrap();
    assert_ne!(no_marker.token, empty_marker.token);
    assert_eq!(
        failure(store.restore_backup(&no_marker.token)).code,
        ErrorCode::BackupChanged
    );
    assert_eq!(
        store.restore_backup(&empty_marker.token).unwrap().payload,
        saved.payload
    );
}

#[test]
fn raw_family_member_changes_and_removal_invalidate_validation() {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let (dir, store) = setup();
        let saved = store.save(None, b"recover").unwrap();
        checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
        let path = filesystem::family_member(&selected(dir.path()), suffix);
        fs::write(&path, b"first raw bytes").unwrap();
        let first = store.read_backup().unwrap();
        fs::write(&path, b"other raw bytes").unwrap();
        assert_eq!(
            failure(store.restore_backup(&first.token)).code,
            ErrorCode::BackupChanged,
            "{suffix}"
        );
        let changed = store.read_backup().unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(
            failure(store.restore_backup(&changed.token)).code,
            ErrorCode::BackupChanged,
            "{suffix}"
        );
        let missing = store.read_backup().unwrap();
        assert_eq!(
            store.restore_backup(&missing.token).unwrap().payload,
            saved.payload
        );
    }
}

#[test]
fn backup_sidecars_are_preserved_and_refused_without_consumption_or_publication() {
    for suffix in ["-journal", "-wal", "-shm"] {
        let (dir, store) = setup();
        let saved = store.save(None, b"backup").unwrap();
        let validated = checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
        let backup_path = checkpoint_path(dir.path());
        let original = fs::read(&backup_path).unwrap();
        let newer = store.save(saved.generation.as_deref(), b"newer").unwrap();
        let selected_before = selected(dir.path());
        let sidecar = filesystem::family_member(&backup_path, suffix);
        fs::write(&sidecar, b"must remain raw").unwrap();
        for result in [
            store.read_backup().map(|_| ()),
            checkpoint(&store, newer.generation.as_deref().unwrap()).map(|_| ()),
            store.restore_backup(&validated.token).map(|_| ()),
        ] {
            let error = failure(result);
            assert_eq!(error.code, ErrorCode::InvalidStoredData, "{suffix}");
            let capture = error.recovery_directory.unwrap();
            assert_eq!(
                capture.parent(),
                Some(dir.path().join("Recovery").as_path())
            );
            assert_eq!(
                fs::read(capture.join(backup_path.file_name().unwrap())).unwrap(),
                original
            );
            assert_eq!(
                fs::read(capture.join(sidecar.file_name().unwrap())).unwrap(),
                b"must remain raw"
            );
        }
        assert_eq!(fs::read(&backup_path).unwrap(), original);
        assert_eq!(fs::read(&sidecar).unwrap(), b"must remain raw");
        assert_eq!(selected(dir.path()), selected_before);
        assert_eq!(store.load().unwrap(), newer);
    }
}

#[test]
fn successful_backup_publication_matches_requested_generation_and_payload() {
    let (dir, store) = setup();
    let original = store.save(None, b"original").unwrap();
    checkpoint(&store, original.generation.as_deref().unwrap()).unwrap();
    let newer = store
        .save(original.generation.as_deref(), b"newer")
        .unwrap();
    let published = checkpoint(&store, newer.generation.as_deref().unwrap()).unwrap();
    assert_eq!(published.snapshot, newer);
    let path = checkpoint_path(dir.path());
    assert_eq!(
        database::verified(&path, &configuration(dir.path())).unwrap(),
        newer
    );
    assert_eq!(store.read_backup().unwrap(), published);
}

#[test]
fn recovery_capture_contains_every_raw_member_and_retains_the_original_family() {
    let (dir, store) = setup();
    store.save(None, b"snapshot").unwrap();
    let path = selected(dir.path());
    for (index, suffix) in ["", "-journal", "-wal", "-shm"].iter().enumerate() {
        fs::write(filesystem::family_member(&path, suffix), [index as u8; 16]).unwrap();
    }
    let capture = filesystem::preserve_family(&path, dir.path()).unwrap();
    for (index, suffix) in ["", "-journal", "-wal", "-shm"].iter().enumerate() {
        let source = filesystem::family_member(&path, suffix);
        assert_eq!(fs::read(&source).unwrap(), [index as u8; 16]);
        assert_eq!(
            fs::read(capture.join(source.file_name().unwrap())).unwrap(),
            [index as u8; 16]
        );
    }
}

#[test]
fn recovery_capture_preserves_read_only_source_bytes_without_changing_its_permissions() {
    let (dir, store) = setup();
    store.save(None, b"snapshot").unwrap();
    let path = selected(dir.path());
    let bytes = fs::read(&path).unwrap();
    let permissions = fs::metadata(&path).unwrap().permissions();
    let mut read_only = permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&path, read_only).unwrap();
    let result = filesystem::preserve_family(&path, dir.path());
    let unchanged = fs::metadata(&path).unwrap().permissions().readonly();
    fs::set_permissions(&path, permissions).unwrap();
    let capture = result.unwrap();
    assert!(unchanged);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        fs::read(capture.join(path.file_name().unwrap())).unwrap(),
        bytes
    );
}

#[test]
fn a_live_backup_wal_is_captured_before_any_sqlite_read_or_replacement() {
    let (dir, store) = setup();
    let saved = store.save(None, b"backup").unwrap();
    checkpoint(&store, saved.generation.as_deref().unwrap()).unwrap();
    let newer = store.save(saved.generation.as_deref(), b"newer").unwrap();
    let path = checkpoint_path(dir.path());
    let external = rusqlite::Connection::open(&path).unwrap();
    external
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    external
        .execute(
            "UPDATE store_state SET generation=?1 WHERE id=1",
            [uuid::Uuid::new_v4().to_string()],
        )
        .unwrap();
    let family = ["", "-wal", "-shm"].map(|suffix| {
        let member = filesystem::family_member(&path, suffix);
        let bytes = fs::read(&member).unwrap();
        (member, bytes)
    });
    let error = failure(checkpoint(&store, newer.generation.as_deref().unwrap()));
    assert_eq!(error.code, ErrorCode::InvalidStoredData);
    let capture = error.recovery_directory.unwrap();
    for (member, bytes) in &family {
        assert_eq!(fs::read(member).unwrap(), *bytes);
        assert_eq!(
            fs::read(capture.join(member.file_name().unwrap())).unwrap(),
            *bytes
        );
    }
    assert_eq!(store.load().unwrap(), newer);
    external.close().unwrap();
}

#[test]
fn reported_failure_keeps_the_only_pre_open_capture_of_each_selected_family() {
    for operation in ["load", "save", "create-backup"] {
        for damage in ["unreadable", "invalid-payload-digest"] {
            let (dir, store) = setup();
            let saved = store.save(None, b"original opaque bytes").unwrap();
            let path = selected(dir.path());
            if damage == "unreadable" {
                fs::write(&path, b"owned fixture is not SQLite").unwrap();
            } else {
                let connection = rusqlite::Connection::open(&path).unwrap();
                connection
                    .execute(
                        "UPDATE store_state SET payload=?1 WHERE id=1",
                        [b"wrong digest".as_slice()],
                    )
                    .unwrap();
                connection.close().unwrap();
            }
            let original = fs::read(&path).unwrap();
            let journal = filesystem::family_member(&path, "-journal");
            let journal_bytes = b"owned unreadable rollback journal";
            fs::write(&journal, journal_bytes).unwrap();
            let result = match operation {
                "load" => store.load().map(|_| ()),
                "save" => store
                    .save(saved.generation.as_deref(), b"must not be accepted")
                    .map(|_| ()),
                "create-backup" => {
                    checkpoint(&store, saved.generation.as_deref().unwrap()).map(|_| ())
                }
                _ => unreachable!(),
            };
            let error = failure(result);
            let recovery = error
                .recovery_directory
                .expect("Failure must expose its pre-open raw capture");
            let captures = fs::read_dir(dir.path().join("Recovery"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            assert_eq!(
                captures.as_slice(),
                std::slice::from_ref(&recovery),
                "{operation}/{damage}: no redundant post-open capture"
            );
            assert_eq!(
                fs::read(recovery.join(path.file_name().unwrap())).unwrap(),
                original,
                "{operation}/{damage}: original database bytes"
            );
            assert_eq!(
                fs::read(recovery.join(journal.file_name().unwrap())).unwrap(),
                journal_bytes,
                "{operation}/{damage}: original journal bytes even if SQLite discarded it"
            );
            assert_eq!(fs::read(&path).unwrap(), original, "{operation}/{damage}");
            if operation != "create-backup" {
                // Exercise the actual journal-discard path, not only a failed open
                // that happened to leave its sidecars untouched.
                assert!(
                    !journal.exists(),
                    "{operation}/{damage}: SQLite discarded the invalid journal"
                );
            }
        }
    }
}

#[test]
fn unrelated_backup_entries_do_not_block_or_invalidate_reconstruction() {
    let (dir, store) = setup();
    let saved = store.save(None, b"verified primary").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let old = checkpoint_path(dir.path());
    let bytes = fs::read(&old).unwrap();
    fs::remove_file(dir.path().join("active-backup")).unwrap();
    let notes = dir.path().join("Backups/notes");
    fs::create_dir(&notes).unwrap();
    let note = notes.join("keep.txt");
    fs::write(&note, b"old").unwrap();
    let token = store.prepare_backup_rebuild(generation).unwrap();
    fs::write(&note, b"edited").unwrap();
    fs::write(dir.path().join("Backups/other.sqlite"), b"unmanaged").unwrap();
    assert_eq!(
        store.rebuild_backup(generation, &token).unwrap().snapshot,
        saved
    );
    assert_eq!(fs::read(old).unwrap(), bytes);
    assert_eq!(fs::read(note).unwrap(), b"edited");
    assert_eq!(
        fs::read(dir.path().join("Backups/other.sqlite")).unwrap(),
        b"unmanaged"
    );
}

#[test]
fn ambiguous_backup_review_still_binds_orphaned_managed_sidecars() {
    let (dir, store) = setup();
    let saved = store.save(None, b"verified primary").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let marker = dir.path().join("active-backup");
    fs::remove_file(&marker).unwrap();
    let token = store.prepare_backup_rebuild(generation).unwrap();
    let sidecar = dir.path().join("Backups/snapshot.sqlite-wal");
    fs::write(&sidecar, b"retained evidence").unwrap();
    assert_eq!(
        failure(store.rebuild_backup(generation, &token)).code,
        ErrorCode::BackupChanged
    );
    assert!(!marker.exists());
    assert_eq!(fs::read(sidecar).unwrap(), b"retained evidence");
}

#[test]
fn reconstruction_does_not_replace_a_damaged_selector_if_capture_failed() {
    let (dir, store) = setup();
    let saved = store.save(None, b"verified primary").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let marker = dir.path().join("active-backup");
    fs::write(&marker, b"damaged selector").unwrap();
    fs::write(dir.path().join("Recovery"), b"non-directory evidence").unwrap();
    let token = store.prepare_backup_rebuild(generation).unwrap();
    assert_eq!(
        failure(store.rebuild_backup(generation, &token)).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(fs::read(marker).unwrap(), b"damaged selector");
    assert_eq!(
        fs::read(dir.path().join("Recovery")).unwrap(),
        b"non-directory evidence"
    );
}

#[test]
fn oversized_sidecar_blocks_live_open_capture_and_restore_review_without_mutation() {
    let (dir, store) = setup();
    let saved = store.save(None, b"verified primary").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&store, generation).unwrap();
    let primary = selected(dir.path());
    let original = fs::read(&primary).unwrap();
    let marker = fs::read(dir.path().join("active-generation")).unwrap();
    let backup_marker = fs::read(dir.path().join("active-backup")).unwrap();
    let sidecar = filesystem::family_member(&primary, "-wal");
    fs::File::create(&sidecar)
        .unwrap()
        .set_len(MAXIMUM_RAW_RECOVERY_BYTES + 1)
        .unwrap();
    assert_eq!(
        failure(store.load()).code,
        ErrorCode::RawRecoveryLimitExceeded
    );
    assert_eq!(
        failure(store.prepare_restore(b"new")).code,
        ErrorCode::RawRecoveryLimitExceeded
    );
    assert_eq!(
        failure(store.read_backup()).code,
        ErrorCode::RawRecoveryLimitExceeded
    );
    assert_eq!(
        failure(store.prepare_backup_rebuild(generation)).code,
        ErrorCode::RawRecoveryLimitExceeded
    );
    assert_eq!(fs::read(primary).unwrap(), original);
    assert_eq!(
        fs::metadata(sidecar).unwrap().len(),
        MAXIMUM_RAW_RECOVERY_BYTES + 1
    );
    assert_eq!(
        fs::read(dir.path().join("active-generation")).unwrap(),
        marker
    );
    assert_eq!(
        fs::read(dir.path().join("active-backup")).unwrap(),
        backup_marker
    );
    assert!(!dir.path().join("Recovery").exists());
}

// Most fixtures need a reviewed healthy checkpoint; race tests pass captured tokens explicitly.
fn checkpoint(store: &ConfigurationStore, generation: &str) -> Result<BackupSnapshot, Failure> {
    let preparation = store.prepare_backup(generation)?;
    store.create_backup(generation, &preparation.token)
}
