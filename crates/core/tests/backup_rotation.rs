use configuration_storage::{BackupSnapshot, Configuration, ConfigurationStore, ErrorCode};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn open(root: &Path) -> ConfigurationStore {
    ConfigurationStore::open(Configuration {
        directory: root.to_owned(),
        owner: "rotation.tests".into(),
        maximum_payload_bytes: 1024,
    })
    .unwrap()
}

fn selected_path(root: &Path, marker: &str, directory: &str) -> PathBuf {
    root.join(directory).join(format!(
        "{}.sqlite",
        fs::read_to_string(root.join(marker)).unwrap()
    ))
}
fn backup_path(root: &Path) -> PathBuf {
    selected_path(root, "active-backup", "Backups")
}
fn primary_path(root: &Path) -> PathBuf {
    selected_path(root, "active-generation", "Generations")
}
fn checkpoint(store: &ConfigurationStore, generation: &str) -> BackupSnapshot {
    let preparation = store.prepare_backup(generation).unwrap();
    store.create_backup(generation, &preparation.token).unwrap()
}
fn replace_payload(path: &Path, payload: &[u8]) {
    let checksum = Sha256::digest(payload)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    rusqlite::Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE store_state SET payload=?1,digest=?2 WHERE id=1",
            rusqlite::params![payload, checksum],
        )
        .unwrap();
}

#[test]
fn selected_backup_observations_keep_exact_fractional_file_dates() {
    use std::time::{Duration, UNIX_EPOCH};

    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let snapshot = store.save(None, b"retained backup").unwrap();
    let generation = snapshot.generation.as_deref().unwrap();
    checkpoint(&store, generation);
    let path = backup_path(fixture.path());
    let original = fs::read(&path).unwrap();
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    for (date, seconds) in [
        (UNIX_EPOCH - Duration::from_nanos(100), -1e-7),
        (UNIX_EPOCH + Duration::from_nanos(200), 2e-7),
    ] {
        file.set_modified(date).unwrap();
        let backup = store.read_backup().unwrap();
        assert_eq!(backup.modified_at, seconds);
        assert_eq!(backup.snapshot, snapshot);
        assert_eq!(
            store.prepare_backup(generation).unwrap().backup,
            Some(backup)
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[test]
fn rotation_rejects_backup_replaced_after_host_review() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("store");
    let store = open(&root);
    let first = store.save(None, b"reviewed backup").unwrap();
    checkpoint(&store, first.generation.as_deref().unwrap());
    let current = store
        .save(first.generation.as_deref(), b"current primary")
        .unwrap();
    let generation = current.generation.as_deref().unwrap();
    let reviewed = store.prepare_backup(generation).unwrap();
    assert_eq!(
        reviewed.backup.unwrap().snapshot.payload.as_deref(),
        Some(b"reviewed backup".as_slice())
    );

    let rival_root = fixture.path().join("rival");
    let rival = open(&rival_root);
    let unreviewed = rival.save(None, b"unreviewed opaque backup").unwrap();
    checkpoint(&rival, unreviewed.generation.as_deref().unwrap());
    let replacement = fs::read(backup_path(&rival_root)).unwrap();
    fs::write(backup_path(&root), &replacement).unwrap();
    let error = store
        .create_backup(generation, &reviewed.token)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BackupChanged);
    assert_eq!(fs::read(backup_path(&root)).unwrap(), replacement);
    assert_eq!(store.load().unwrap(), current);
}

#[test]
fn preparation_exposes_opaque_future_and_malformed_product_payloads_without_rotation() {
    for payload in [
        br#"{"version":999,"unknown":"retain me"}"#.as_slice(),
        b"not product JSON",
        b"",
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let store = open(fixture.path());
        let first = store.save(None, payload).unwrap();
        checkpoint(&store, first.generation.as_deref().unwrap());
        let current = store
            .save(first.generation.as_deref(), b"valid current product")
            .unwrap();
        let path = backup_path(fixture.path());
        let raw = fs::read(&path).unwrap();
        let preparation = store
            .prepare_backup(current.generation.as_deref().unwrap())
            .unwrap();
        let backup = preparation.backup.unwrap();
        assert_eq!(backup.snapshot, first);
        assert_eq!(backup.snapshot.payload.as_deref(), Some(payload));
        assert_eq!(backup, store.read_backup().unwrap());
        // A host that rejects these bytes simply withholds create_backup: preparation
        // neither rotates nor routes opaque product data through raw-damage rebuild.
        assert_eq!(fs::read(path).unwrap(), raw);
        assert_eq!(store.load().unwrap(), current);
        assert!(!fixture.path().join("Recovery").exists());
    }
}

#[test]
fn absence_is_reviewed_and_appearance_invalidates_it() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("store");
    let store = open(&root);
    let current = store.save(None, b"current").unwrap();
    let generation = current.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    assert!(preparation.backup.is_none());
    assert!(!root.join("Backups").exists());
    let rival_root = fixture.path().join("rival");
    let rival = open(&rival_root);
    let rival_snapshot = rival.save(None, b"new unreviewed backup").unwrap();
    checkpoint(&rival, rival_snapshot.generation.as_deref().unwrap());
    fs::create_dir(root.join("Backups")).unwrap();
    let path = root.join("Backups/snapshot.sqlite");
    let raw = fs::read(backup_path(&rival_root)).unwrap();
    fs::write(&path, &raw).unwrap();
    assert_eq!(
        store
            .create_backup(generation, &preparation.token)
            .unwrap_err()
            .code,
        ErrorCode::BackupChanged
    );
    assert_eq!(fs::read(path).unwrap(), raw);
}

#[test]
fn backup_created_by_another_handle_invalidates_prepared_absence() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let saved = store.save(None, &[0, 255, 128]).unwrap();
    let generation = saved.generation.as_deref().unwrap();
    let absent = store.prepare_backup(generation).unwrap();
    assert!(absent.backup.is_none());

    let other = open(fixture.path());
    let created = checkpoint(&other, generation);
    assert_eq!(created.snapshot, saved);
    assert_eq!(
        store
            .create_backup(generation, &absent.token)
            .unwrap_err()
            .code,
        ErrorCode::BackupChanged
    );
    assert_eq!(store.read_backup().unwrap(), created);
    assert_eq!(store.load().unwrap(), saved);
}

#[test]
fn same_snapshot_rebuild_invalidates_older_rotation_preparation() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let saved = store.save(None, &[42, 0, 254]).unwrap();
    let generation = saved.generation.as_deref().unwrap();
    let original = checkpoint(&store, generation);
    let reviewed = store.prepare_backup(generation).unwrap();
    assert_eq!(reviewed.backup.as_ref(), Some(&original));

    let rebuild = store.prepare_backup_rebuild(generation).unwrap();
    let replacement = store.rebuild_backup(generation, &rebuild).unwrap();
    assert_eq!(replacement.snapshot, original.snapshot);
    assert_ne!(replacement.token, original.token);
    assert_eq!(
        store
            .create_backup(generation, &reviewed.token)
            .unwrap_err()
            .code,
        ErrorCode::BackupChanged
    );
    assert_eq!(store.read_backup().unwrap(), replacement);
    assert_eq!(store.load().unwrap(), saved);

    let fresh = store.prepare_backup(generation).unwrap();
    assert_eq!(fresh.backup.as_ref(), Some(&replacement));
    assert_eq!(
        store.create_backup(generation, &fresh.token).unwrap(),
        replacement
    );
}

#[test]
fn backup_family_removal_selector_or_sidecar_change_rejects_rotation() {
    for change in ["removal", "selector", "journal", "wal", "shm"] {
        let fixture = tempfile::tempdir().unwrap();
        let store = open(fixture.path());
        let first = store.save(None, b"old").unwrap();
        checkpoint(&store, first.generation.as_deref().unwrap());
        let current = store.save(first.generation.as_deref(), b"current").unwrap();
        let generation = current.generation.as_deref().unwrap();
        let preparation = store.prepare_backup(generation).unwrap();
        let path = backup_path(fixture.path());
        match change {
            "removal" => fs::remove_file(&path).unwrap(),
            "selector" => {
                fs::write(fixture.path().join("active-backup"), b"invalid selector").unwrap()
            }
            suffix => fs::write(
                format!("{}-{suffix}", path.display()),
                b"unreviewed sidecar",
            )
            .unwrap(),
        }
        assert_eq!(
            store
                .create_backup(generation, &preparation.token)
                .unwrap_err()
                .code,
            ErrorCode::BackupChanged,
            "{change}"
        );
        assert_eq!(store.load().unwrap(), current);
        // Revision checking must precede a changed backup's SQLite open/recovery.
        assert!(!fixture.path().join("Recovery").exists());
    }
}

#[test]
fn primary_changes_after_review_reject_rotation_without_touching_backup() {
    for reuse_nonce in [false, true] {
        let fixture = tempfile::tempdir().unwrap();
        let store = open(fixture.path());
        let first = store.save(None, b"old").unwrap();
        checkpoint(&store, first.generation.as_deref().unwrap());
        let current = store
            .save(first.generation.as_deref(), b"reviewed primary")
            .unwrap();
        let generation = current.generation.as_deref().unwrap();
        let preparation = store.prepare_backup(generation).unwrap();
        let path = backup_path(fixture.path());
        let raw = fs::read(&path).unwrap();
        if reuse_nonce {
            replace_payload(&primary_path(fixture.path()), b"unreviewed primary");
        } else {
            store.save(Some(generation), b"unreviewed primary").unwrap();
        }
        assert_eq!(
            store
                .create_backup(generation, &preparation.token)
                .unwrap_err()
                .code,
            ErrorCode::BackupChanged
        );
        assert_eq!(fs::read(path).unwrap(), raw);
    }
}

#[test]
fn cached_primary_generation_cannot_prepare_a_changed_valid_digest_payload() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let original = store.save(None, b"host validated primary").unwrap();
    replace_payload(
        &primary_path(fixture.path()),
        b"new valid digest with same stored nonce",
    );
    assert_eq!(
        store
            .prepare_backup(original.generation.as_deref().unwrap())
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_ne!(store.load().unwrap().generation, original.generation);
    assert!(!fixture.path().join("Backups").exists());
}

#[test]
fn rotation_tokens_are_required_and_distinct_from_restore_and_rebuild_tokens() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let current = store.save(None, b"current").unwrap();
    let generation = current.generation.as_deref().unwrap();
    let backup = checkpoint(&store, generation);
    let preparation = store.prepare_backup(generation).unwrap();
    assert_eq!(preparation.backup.as_ref(), Some(&backup));
    let rebuild = store.prepare_backup_rebuild(generation).unwrap();
    let portable_restore = store.prepare_restore(b"current").unwrap();
    for token in ["", "wrong", &backup.token, &rebuild, &portable_restore] {
        assert_eq!(
            store.create_backup(generation, token).unwrap_err().code,
            ErrorCode::BackupChanged
        );
    }
    assert_eq!(
        store
            .rebuild_backup(generation, &preparation.token)
            .unwrap_err()
            .code,
        ErrorCode::BackupChanged
    );
    assert_eq!(
        store.restore_backup(&preparation.token).unwrap_err().code,
        ErrorCode::BackupChanged
    );
    // A successful no-op still requires a matching review, and does not consume it.
    assert_eq!(
        store.create_backup(generation, &preparation.token).unwrap(),
        backup
    );
    assert_eq!(
        store.create_backup(generation, &preparation.token).unwrap(),
        backup
    );
}

#[test]
fn prepared_nested_backup_token_restores_the_original_selection() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let original = store.save(None, b"reviewed original backup").unwrap();
    checkpoint(&store, original.generation.as_deref().unwrap());
    let current = store
        .save(original.generation.as_deref(), b"current primary")
        .unwrap();
    let preparation = store
        .prepare_backup(current.generation.as_deref().unwrap())
        .unwrap();
    let backup = preparation.backup.unwrap();
    assert_eq!(backup.snapshot, original);
    assert_eq!(
        store.restore_backup(&preparation.token).unwrap_err().code,
        ErrorCode::BackupChanged
    );
    let primary = primary_path(fixture.path());
    let raw = fs::read(&primary).unwrap();
    // The nested token binds the live original selection, not the temporary copy
    // used to capture its payload during preparation.
    let restored = store.restore_backup(&backup.token).unwrap();
    assert_eq!(restored.payload, original.payload);
    assert_ne!(restored.generation, original.generation);
    assert_ne!(restored.generation, current.generation);
    assert_eq!(store.load().unwrap(), restored);
    assert_eq!(fs::read(primary).unwrap(), raw);
}

#[test]
fn replaced_backup_type_is_refused_without_overwriting_retained_bytes() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let original = store.save(None, b"original backup").unwrap();
    checkpoint(&store, original.generation.as_deref().unwrap());
    let current = store
        .save(original.generation.as_deref(), b"current primary")
        .unwrap();
    let generation = current.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    let path = backup_path(fixture.path());
    let raw = fs::read(&path).unwrap();
    let retained = fixture.path().join("retained.sqlite");
    fs::rename(&path, &retained).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        store
            .create_backup(generation, &preparation.token)
            .unwrap_err()
            .code,
        ErrorCode::InvalidStoredData
    );
    assert!(path.is_dir());
    assert_eq!(fs::read(retained).unwrap(), raw);
    assert_eq!(store.load().unwrap(), current);
}

#[cfg(unix)]
#[test]
fn byte_identical_link_target_cannot_bypass_backup_revision_checks() {
    use std::os::unix::fs::symlink;
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let original = store.save(None, b"original backup").unwrap();
    checkpoint(&store, original.generation.as_deref().unwrap());
    let current = store
        .save(original.generation.as_deref(), b"current primary")
        .unwrap();
    let generation = current.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    let path = backup_path(fixture.path());
    let target = fixture.path().join("unmanaged.sqlite");
    let raw = fs::read(&path).unwrap();
    fs::rename(&path, &target).unwrap();
    symlink(&target, &path).unwrap();
    assert_eq!(
        store
            .create_backup(generation, &preparation.token)
            .unwrap_err()
            .code,
        ErrorCode::InvalidStoredData
    );
    assert!(path.is_symlink());
    assert_eq!(fs::read(target).unwrap(), raw);
    assert_eq!(store.load().unwrap(), current);
}

#[test]
fn no_op_requires_unchanged_raw_backup_even_when_payload_and_generation_match() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let current = store.save(None, b"same bytes").unwrap();
    let generation = current.generation.as_deref().unwrap();
    checkpoint(&store, generation);
    let preparation = store.prepare_backup(generation).unwrap();
    let path = backup_path(fixture.path());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version=99")
        .unwrap();
    let raw = fs::read(&path).unwrap();
    assert_eq!(
        store
            .create_backup(generation, &preparation.token)
            .unwrap_err()
            .code,
        ErrorCode::BackupChanged
    );
    assert_eq!(fs::read(path).unwrap(), raw);
}

#[test]
fn exact_byte_replacement_is_same_opaque_revision() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let first = store.save(None, b"reviewed").unwrap();
    checkpoint(&store, first.generation.as_deref().unwrap());
    let current = store.save(first.generation.as_deref(), b"current").unwrap();
    let generation = current.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    let path = backup_path(fixture.path());
    let raw = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, raw).unwrap();
    assert_eq!(
        store
            .create_backup(generation, &preparation.token)
            .unwrap()
            .snapshot,
        current
    );
}

#[test]
fn successful_rotation_retires_old_review_but_retains_restore_separation() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    let first = store.save(None, b"old").unwrap();
    checkpoint(&store, first.generation.as_deref().unwrap());
    let current = store.save(first.generation.as_deref(), b"current").unwrap();
    let generation = current.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    let rotated = store.create_backup(generation, &preparation.token).unwrap();
    assert_eq!(rotated.snapshot, current);
    assert_eq!(
        store
            .create_backup(generation, &preparation.token)
            .unwrap_err()
            .code,
        ErrorCode::BackupChanged
    );
    assert_ne!(rotated.token, preparation.token);
    assert_eq!(store.read_backup().unwrap(), rotated);
}

#[test]
fn uninitialized_preparation_and_wrong_tokens_do_not_create_backup_files() {
    let fixture = tempfile::tempdir().unwrap();
    let store = open(fixture.path());
    assert_eq!(
        store.prepare_backup("unknown").unwrap_err().code,
        ErrorCode::NotInitialized
    );
    assert_eq!(
        store.create_backup("unknown", "wrong").unwrap_err().code,
        ErrorCode::BackupChanged
    );
    assert!(!fixture.path().join("Backups").exists());
}
