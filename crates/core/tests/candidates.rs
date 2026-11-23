use configuration_storage::{
    Configuration, ConfigurationStore, ErrorCode, MAXIMUM_CANDIDATES_PER_PAGE,
    MAXIMUM_RAW_RECOVERY_BYTES,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};
use uuid::Uuid;

fn store(root: &Path) -> ConfigurationStore {
    ConfigurationStore::open(Configuration {
        directory: root.to_path_buf(),
        owner: "candidate.tests".into(),
        maximum_payload_bytes: 4096,
    })
    .unwrap()
}
fn selected(root: &Path) -> String {
    fs::read_to_string(root.join("active-generation")).unwrap()
}
fn path(root: &Path, id: &str) -> PathBuf {
    root.join("Generations").join(format!("{id}.sqlite"))
}
fn family(file: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    ["", "-journal", "-wal", "-shm"]
        .into_iter()
        .filter_map(|suffix| {
            let path = file.with_file_name(format!(
                "{}{suffix}",
                file.file_name().unwrap().to_str().unwrap()
            ));
            path.exists()
                .then(|| (path.clone(), fs::read(path).unwrap()))
        })
        .collect()
}
fn unchanged(family: &[(PathBuf, Vec<u8>)]) {
    for (path, bytes) in family {
        assert_eq!(&fs::read(path).unwrap(), bytes);
    }
}

fn rollback_journal(file: &Path, super_name: Option<&str>) -> Vec<u8> {
    let db = fs::read(file).unwrap();
    let encoded = u16::from_be_bytes(db[16..18].try_into().unwrap());
    let page = if encoded == 1 {
        65536
    } else {
        u32::from(encoded)
    };
    let magic = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    let mut journal = Vec::from(magic);
    for value in [0, 0, db.len() as u32 / page, 512, page] {
        journal.extend_from_slice(&value.to_be_bytes());
    }
    journal.resize(512, 0);
    if let Some(name) = super_name {
        journal.extend_from_slice(&(0x40000000_u32 / page + 1).to_be_bytes());
        journal.extend_from_slice(name.as_bytes());
        journal.extend_from_slice(&(name.len() as u32).to_be_bytes());
        journal.extend_from_slice(&name.bytes().map(u32::from).sum::<u32>().to_be_bytes());
        journal.extend_from_slice(&magic);
    }
    journal
}

#[test]
fn copied_standalone_rollback_recovery_preserves_the_original_journal() {
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    let saved = writer.save(None, b"accepted original").unwrap();
    let id = selected(root.path());
    let file = path(root.path(), &id);
    let journal = file.with_file_name(format!("{id}.sqlite-journal"));
    fs::write(journal, rollback_journal(&file, None)).unwrap();
    let original = family(&file);
    assert_eq!(writer.read_snapshot_candidate(&id).unwrap().snapshot, saved);
    unchanged(&original);
}

#[test]
fn live_and_copied_super_journal_references_are_refused_without_changing_evidence() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("store");
    let writer = store(&store_root);
    let saved = writer.save(None, b"accepted original").unwrap();
    let id = selected(&store_root);
    let file = path(&store_root, &id).canonicalize().unwrap();
    let external = root.path().join("attached-mj0000009aa");
    let journal = file.with_file_name(format!("{id}.sqlite-journal"));
    let external = external
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join(external.file_name().unwrap());
    let mut external_bytes = journal.to_str().unwrap().as_bytes().to_vec();
    external_bytes.push(0);
    fs::write(&external, &external_bytes).unwrap();
    let mut bytes = rollback_journal(&file, Some(external.to_str().unwrap()));
    for malformed_checksum in [false, true] {
        if malformed_checksum {
            let index = bytes.len() - 12;
            bytes[index] ^= 1;
        }
        fs::write(&journal, &bytes).unwrap();
        let original = family(&file);
        assert_eq!(
            writer.read_snapshot_candidate(&id).unwrap_err().code,
            ErrorCode::InvalidStoredData
        );
        let failure = writer.load().unwrap_err();
        assert_eq!(failure.code, ErrorCode::InvalidStoredData);
        let evidence = failure.recovery_directory.unwrap();
        assert_eq!(
            fs::read(evidence.join(journal.file_name().unwrap())).unwrap(),
            bytes
        );
        assert_eq!(
            writer
                .save(saved.generation.as_deref(), b"new edit")
                .unwrap_err()
                .code,
            ErrorCode::InvalidStoredData
        );
        unchanged(&original);
        assert_eq!(fs::read(&external).unwrap(), external_bytes);
    }
}

#[test]
fn empty_inspection_does_not_initialize_or_select_data() {
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    let page = writer.list_snapshot_candidates(64, None, None).unwrap();
    assert!(page.candidates.is_empty() && page.next_cursor.is_none());
    assert_eq!(
        writer
            .read_snapshot_candidate(&Uuid::new_v4().to_string())
            .unwrap_err()
            .code,
        ErrorCode::SnapshotCandidateMissing
    );
    assert!(writer.load().unwrap().payload.is_none());
    assert!(!root.path().join("Generations").exists());
    assert!(!root.path().join("Recovery").exists());
}

#[test]
fn retained_latest_bytes_can_be_reviewed_and_explicitly_restored_again() {
    use configuration_storage::SnapshotCandidateSource::{Backup, Generation};

    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    let first = writer.save(None, b"previous checkpoint").unwrap();
    checkpoint(&writer, first.generation.as_deref().unwrap()).unwrap();
    let latest = writer
        .save(first.generation.as_deref(), &[128, 0, 66, 254])
        .unwrap();
    let retained = selected(root.path());
    let original_path = path(root.path(), &retained);
    let original = family(&original_path);
    let before = writer.list_snapshot_candidates(64, None, None).unwrap();
    let generation_entries: Vec<_> = before
        .candidates
        .iter()
        .filter(|entry| entry.source == Generation)
        .collect();
    assert_eq!(generation_entries.len(), 1);
    assert_eq!(generation_entries[0].id, retained);
    assert_eq!(generation_entries[0].is_selected, Some(true));
    let backup = writer.read_backup().unwrap();
    let backup_entry = before
        .candidates
        .iter()
        .find(|entry| entry.source == Backup)
        .unwrap();
    let inspected_backup = writer.read_snapshot_candidate(&backup_entry.id).unwrap();
    assert_eq!(inspected_backup.source, Backup);
    assert_eq!(inspected_backup.is_selected, Some(true));
    assert_eq!(inspected_backup.snapshot, backup.snapshot);
    let restored = writer.restore_backup(&backup.token).unwrap();
    let current = selected(root.path());
    let page = writer.list_snapshot_candidates(64, None, None).unwrap();
    assert_eq!(page.candidates.len(), 3);
    assert_eq!(
        page.candidates
            .iter()
            .find(|x| x.id == retained)
            .unwrap()
            .is_selected,
        Some(false)
    );
    assert_eq!(
        page.candidates
            .iter()
            .find(|x| x.id == current)
            .unwrap()
            .is_selected,
        Some(true)
    );
    let timestamp = Duration::from_millis(1250);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&original_path)
        .unwrap();
    for (date, seconds) in [
        (UNIX_EPOCH - timestamp, -1.25),
        (UNIX_EPOCH - Duration::from_nanos(100), -1e-7),
        (UNIX_EPOCH, 0.0),
        (UNIX_EPOCH + Duration::from_nanos(200), 2e-7),
        (UNIX_EPOCH + timestamp, 1.25),
    ] {
        file.set_modified(date).unwrap();
        let candidate = writer.read_snapshot_candidate(&retained).unwrap();
        assert_eq!(candidate.snapshot, latest);
        assert_eq!(candidate.source, Generation);
        assert_eq!(candidate.id, retained);
        assert_eq!(candidate.is_selected, Some(false));
        assert_eq!(candidate.modified_at, seconds);
        assert_eq!(selected(root.path()), current);
        unchanged(&original);
    }
    assert_eq!(writer.load().unwrap(), restored);
    assert_eq!(
        writer
            .save(latest.generation.as_deref(), b"stale writer")
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // The host accepts these held bytes; inspection alone never authorizes a restore.
    let payload = writer
        .read_snapshot_candidate(&retained)
        .unwrap()
        .snapshot
        .payload
        .unwrap();
    let review = writer.prepare_restore(&payload).unwrap();
    let recovered = writer.restore_snapshot(&payload, &review).unwrap();
    assert_eq!(recovered.payload, latest.payload);
    assert_ne!(recovered.generation, latest.generation);
    assert_ne!(recovered.generation, restored.generation);
    assert_eq!(store(root.path()).load().unwrap(), recovered);
    unchanged(&original);
}

#[test]
fn inspection_holds_the_read_bytes_without_advancing_a_writer_baseline() {
    let root = tempfile::tempdir().unwrap();
    let original_writer = store(root.path());
    let first = original_writer.save(None, &[1]).unwrap();
    let writer = store(root.path());
    let second = writer.save(first.generation.as_deref(), &[2, 255]).unwrap();
    let id = selected(root.path());
    let held = original_writer.read_snapshot_candidate(&id).unwrap();
    assert_eq!(held.snapshot, second);
    assert_eq!(
        original_writer
            .save(first.generation.as_deref(), &[3])
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let newer = writer.save(second.generation.as_deref(), &[4]).unwrap();
    let reread = original_writer.read_snapshot_candidate(&id).unwrap();
    assert_eq!(reread.id, held.id);
    assert_eq!(reread.snapshot, newer);
    assert_eq!(held.snapshot, second);
    assert_eq!(selected(root.path()), id);
}

#[test]
fn missing_malformed_and_oversized_selectors_remain_unknown_without_adoption() {
    for marker in [None, Some(vec![0xff, 0]), Some(vec![b'x'; 4097])] {
        let root = tempfile::tempdir().unwrap();
        let writer = store(root.path());
        let saved = writer.save(None, b"accepted data").unwrap();
        let id = selected(root.path());
        let original = family(&path(root.path(), &id));
        let selector = root.path().join("active-generation");
        if let Some(bytes) = &marker {
            fs::write(&selector, bytes).unwrap();
        } else {
            fs::remove_file(&selector).unwrap();
        }
        assert_eq!(
            writer.load().unwrap_err().code,
            ErrorCode::InvalidStoredData
        );
        let page = writer.list_snapshot_candidates(1, None, None).unwrap();
        assert_eq!(page.candidates[0].id, id);
        assert_eq!(page.candidates[0].is_selected, None);
        let read = writer.read_snapshot_candidate(&id).unwrap();
        assert_eq!(read.snapshot, saved);
        assert_eq!(read.is_selected, None);
        assert_eq!(fs::read(selector).ok(), marker);
        assert!(!root.path().join("Recovery").exists());
        unchanged(&original);
    }
}

#[test]
fn pagination_is_bounded_ordered_and_includes_only_regular_canonical_files() {
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    fs::create_dir(root.path().join("Generations")).unwrap();
    let mut expected = Vec::new();
    for _ in 0..260 {
        let id = Uuid::new_v4().to_string();
        fs::write(path(root.path(), &id), []).unwrap();
        expected.push(id);
    }
    expected.sort();
    let ignored = root.path().join("Generations/unknown.sqlite");
    fs::write(&ignored, b"untouched").unwrap();
    fs::create_dir(path(root.path(), &Uuid::new_v4().to_string())).unwrap();
    let first = writer
        .list_snapshot_candidates(MAXIMUM_CANDIDATES_PER_PAGE, None, None)
        .unwrap();
    assert_eq!(first.candidates.len(), MAXIMUM_CANDIDATES_PER_PAGE);
    assert_eq!(first.next_cursor, Some(expected[255].clone()));
    let last = writer
        .list_snapshot_candidates(256, first.next_cursor.as_deref(), None)
        .unwrap();
    assert_eq!(
        last.candidates.iter().map(|x| &x.id).collect::<Vec<_>>(),
        expected[256..].iter().collect::<Vec<_>>()
    );
    assert!(last.next_cursor.is_none());
    let mut cursor = None;
    let mut seen = Vec::new();
    loop {
        let page = writer
            .list_snapshot_candidates(17, cursor.as_deref(), None)
            .unwrap();
        seen.extend(page.candidates.into_iter().map(|x| {
            assert_eq!(x.is_selected, None);
            x.id
        }));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, expected);
    let exhausted = writer
        .list_snapshot_candidates(1, expected.last().map(String::as_str), None)
        .unwrap();
    assert!(exhausted.candidates.is_empty() && exhausted.next_cursor.is_none());
    assert_eq!(fs::read(ignored).unwrap(), b"untouched");
    assert_eq!(
        writer
            .save(None, b"must not adopt unselected data")
            .unwrap_err()
            .code,
        ErrorCode::InvalidStoredData
    );
}

#[test]
fn invalid_ids_limits_and_missing_files_never_escape_the_managed_directory() {
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    for id in [
        "",
        "..",
        "../other.sqlite",
        "/tmp/other",
        "00000000-0000-0000-0000-000000000000.sqlite",
        "A0000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(
            writer.read_snapshot_candidate(id).unwrap_err().code,
            ErrorCode::InvalidConfiguration
        );
        assert_eq!(
            writer
                .list_snapshot_candidates(64, Some(id), None)
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfiguration
        );
    }
    for limit in [0, 257, usize::MAX] {
        assert_eq!(
            writer
                .list_snapshot_candidates(limit, None, None)
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfiguration
        );
    }
    let saved = writer.save(None, b"accepted").unwrap();
    let id = selected(root.path());
    fs::remove_file(path(root.path(), &id)).unwrap();
    assert_eq!(
        writer.read_snapshot_candidate(&id).unwrap_err().code,
        ErrorCode::SnapshotCandidateMissing
    );
    assert!(!root.path().join("Recovery").exists());
    assert!(saved.payload.is_some());
}

#[test]
fn corrupt_foreign_and_future_candidates_do_not_poison_other_references() {
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    let saved = writer.save(None, b"accepted").unwrap();
    let active = path(root.path(), &selected(root.path()));
    for (kind, expected) in [
        ("corrupt", ErrorCode::InvalidStoredData),
        ("foreign", ErrorCode::OwnerMismatch),
        ("future", ErrorCode::UnsupportedFormatVersion),
    ] {
        let id = Uuid::new_v4().to_string();
        let file = path(root.path(), &id);
        fs::copy(&active, &file).unwrap();
        if kind == "corrupt" {
            fs::write(&file, b"not SQLite").unwrap();
        } else {
            let db = rusqlite::Connection::open(&file).unwrap();
            db.execute_batch(if kind == "foreign" {
                "UPDATE store_state SET owner='other.owner'"
            } else {
                "PRAGMA user_version=2"
            })
            .unwrap();
        }
        let original = family(&file);
        assert!(
            writer
                .list_snapshot_candidates(64, None, None)
                .unwrap()
                .candidates
                .iter()
                .any(|x| x.id == id)
        );
        assert_eq!(
            writer.read_snapshot_candidate(&id).unwrap_err().code,
            expected
        );
        unchanged(&original);
        assert_eq!(writer.load().unwrap(), saved);
    }
    assert!(!root.path().join("Recovery").exists());
}

#[test]
fn inspection_reads_committed_wal_without_touching_any_original_family_member() {
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    writer.save(None, b"old").unwrap();
    let id = selected(root.path());
    let file = path(root.path(), &id);
    let db = rusqlite::Connection::open(&file).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    let payload = b"committed WAL payload";
    let checksum: String = Sha256::digest(payload)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    db.execute(
        "UPDATE store_state SET payload=?1,digest=?2,generation=?3",
        rusqlite::params![payload, checksum, Uuid::new_v4().to_string()],
    )
    .unwrap();
    let original = family(&file);
    assert!(
        original
            .iter()
            .any(|(p, _)| p.to_string_lossy().ends_with("-wal"))
    );
    let read = writer.read_snapshot_candidate(&id).unwrap();
    assert_eq!(read.snapshot.payload.as_deref(), Some(payload.as_slice()));
    unchanged(&original);
    assert!(!root.path().join("Recovery").exists());
    db.close().unwrap();
}

#[test]
fn oversized_raw_main_or_combined_family_is_rejected_before_materialization() {
    for (oversized_main, size) in [
        (true, MAXIMUM_RAW_RECOVERY_BYTES + 1),
        (false, MAXIMUM_RAW_RECOVERY_BYTES),
    ] {
        let root = tempfile::tempdir().unwrap();
        let writer = store(root.path());
        writer.save(None, b"accepted").unwrap();
        let id = selected(root.path());
        let file = path(root.path(), &id);
        let original = fs::read(&file).unwrap();
        let target = if oversized_main {
            file.clone()
        } else {
            file.with_file_name(format!("{id}.sqlite-wal"))
        };
        let mut oversized = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&target)
            .unwrap();
        oversized.set_len(size).unwrap();
        if !oversized_main {
            oversized.write_all(&[42]).unwrap();
        }
        oversized.seek(SeekFrom::End(-1)).unwrap();
        oversized.write_all(&[254]).unwrap();
        drop(oversized);
        assert_eq!(
            writer.read_snapshot_candidate(&id).unwrap_err().code,
            ErrorCode::RawRecoveryLimitExceeded
        );
        let mut oversized = fs::File::open(&target).unwrap();
        assert_eq!(oversized.metadata().unwrap().len(), size);
        let mut sentinel = [0];
        oversized.seek(SeekFrom::End(-1)).unwrap();
        oversized.read_exact(&mut sentinel).unwrap();
        assert_eq!(sentinel, [254]);
        if !oversized_main {
            oversized.rewind().unwrap();
            oversized.read_exact(&mut sentinel).unwrap();
            assert_eq!(sentinel, [42]);
            assert_eq!(fs::read(&file).unwrap(), original);
        }
        assert_eq!(selected(root.path()), id);
        assert!(!root.path().join("Recovery").exists());
    }
}

#[test]
fn inspection_obeys_the_existing_root_lock() {
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    writer.save(None, b"accepted").unwrap();
    let id = selected(root.path());
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("store.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert_eq!(
        writer
            .list_snapshot_candidates(64, None, None)
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(
        writer.read_snapshot_candidate(&id).unwrap_err().code,
        ErrorCode::Busy
    );
}

#[cfg(target_os = "macos")]
#[test]
fn read_only_existing_lock_keeps_managed_inspection_available_and_exclusive() {
    use configuration_storage::SnapshotCandidateSource::{Backup, Generation};
    use std::{
        io::{ErrorKind, Write},
        os::unix::fs::{MetadataExt, PermissionsExt},
    };

    // SAFETY: geteuid takes no pointers and has no side effects.
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "This fixture requires a non-root user"
    );
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    let configuration = writer.configuration().clone();
    let older = writer.save(None, b"checkpoint fixture").unwrap();
    let backup = checkpoint(&writer, older.generation.as_deref().unwrap()).unwrap();
    let saved = writer
        .save(older.generation.as_deref(), b"current fixture")
        .unwrap();
    let active = selected(root.path());
    let checkpoint = fs::read_to_string(root.path().join("active-backup")).unwrap();
    drop(writer);

    let lock_path = root.path().join("store.lock");
    let mut existing_writer = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    existing_writer.write_all(b"lock fixture sentinel").unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o400)).unwrap();
    let denied = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect_err("Fixture must reject a new writable lock descriptor");
    assert_eq!(denied.kind(), ErrorKind::PermissionDenied);

    let identity = |file: &str| {
        let file = root.path().join(file);
        let metadata = fs::symlink_metadata(&file).unwrap();
        assert!(metadata.is_file());
        (
            fs::read(file).unwrap(),
            metadata.dev(),
            metadata.ino(),
            metadata.mode() & 0o7777,
        )
    };
    let markers = [
        "store.lock",
        "store-owner",
        "active-generation",
        "active-backup",
    ];
    let before = markers.map(identity);
    assert_eq!(before[0].3, 0o400);
    let database_families = || {
        let mut files = family(&path(root.path(), &active));
        files.extend(family(
            &root
                .path()
                .join("Backups")
                .join(format!("{checkpoint}.sqlite")),
        ));
        files
    };
    let original_families = database_families();

    ConfigurationStore::open(configuration.clone()).unwrap();
    let reopened =
        ConfigurationStore::open_existing(configuration.clone()).unwrap_or_else(|error| {
            panic!("A readable existing lock must allow existing-store admission: {error:?}")
        });
    let checkpoint_id = format!("backup:{checkpoint}");
    for (source, id, expected) in [
        (Generation, active.as_str(), &saved),
        (Backup, checkpoint_id.as_str(), &backup.snapshot),
    ] {
        let page = reopened
            .list_snapshot_candidates(8, None, Some(source))
            .unwrap();
        let [entry] = page.candidates.as_slice() else {
            panic!("Expected exactly one candidate in {source:?}");
        };
        assert!(page.next_cursor.is_none());
        assert_eq!(entry.id, id);
        assert_eq!(entry.is_selected, Some(true));
        assert_eq!(
            &reopened.read_snapshot_candidate(id).unwrap().snapshot,
            expected
        );
    }

    existing_writer.try_lock().unwrap();
    assert_eq!(
        ConfigurationStore::open(configuration).err().unwrap().code,
        ErrorCode::Busy
    );
    assert_eq!(
        reopened
            .list_snapshot_candidates(8, None, Some(Generation))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(
        reopened.read_snapshot_candidate(&active).unwrap_err().code,
        ErrorCode::Busy
    );
    drop(existing_writer);
    assert_eq!(
        reopened.read_snapshot_candidate(&active).unwrap().snapshot,
        saved
    );
    assert_eq!(markers.map(identity), before);
    assert_eq!(database_families(), original_families);
}

#[cfg(unix)]
#[test]
fn candidate_links_are_omitted_and_rejected_without_following_their_target() {
    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    writer.save(None, b"accepted").unwrap();
    let id = selected(root.path());
    let file = path(root.path(), &id);
    let bytes = fs::read(&file).unwrap();
    let linked = Uuid::new_v4().to_string();
    std::os::unix::fs::symlink(&file, path(root.path(), &linked)).unwrap();
    assert!(
        !writer
            .list_snapshot_candidates(64, None, None)
            .unwrap()
            .candidates
            .iter()
            .any(|x| x.id == linked)
    );
    assert_eq!(
        writer.read_snapshot_candidate(&linked).unwrap_err().code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(fs::read(file).unwrap(), bytes);
}

#[test]
fn retained_backup_can_restore_when_primary_and_selected_backup_are_damaged() {
    use configuration_storage::SnapshotCandidateSource::{Backup, Generation};

    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    let a = writer.save(None, b"retained A").unwrap();
    checkpoint(&writer, a.generation.as_deref().unwrap()).unwrap();
    let marker = root.path().join("active-backup");
    let old_stem = fs::read_to_string(&marker).unwrap();
    let old_backup = root
        .path()
        .join("Backups")
        .join(format!("{old_stem}.sqlite"));
    let b = writer.save(a.generation.as_deref(), b"current B").unwrap();
    let generation = b.generation.as_deref().unwrap();
    let review = writer.prepare_backup_rebuild(generation).unwrap();
    writer.rebuild_backup(generation, &review).unwrap();
    let new_stem = fs::read_to_string(&marker).unwrap();
    assert_ne!(new_stem, old_stem);
    let damaged_backup = root
        .path()
        .join("Backups")
        .join(format!("{new_stem}.sqlite"));
    let damaged_primary = path(root.path(), &selected(root.path()));
    fs::write(&damaged_backup, b"damaged backup").unwrap();
    fs::write(&damaged_primary, b"damaged primary").unwrap();
    // A duplicate UUID across managed locations must not alias either file.
    fs::copy(&damaged_primary, path(root.path(), &old_stem)).unwrap();
    let original = family(&old_backup);
    assert!(writer.read_backup().is_err());

    let mut cursor = None;
    let mut candidates = Vec::new();
    loop {
        let page = writer
            .list_snapshot_candidates(1, cursor.as_deref(), None)
            .unwrap();
        candidates.extend(page.candidates);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(candidates.len(), 4);
    assert!(candidates.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert!(
        candidates
            .iter()
            .any(|c| c.id == old_stem && c.source == Generation)
    );
    let old = candidates
        .iter()
        .find(|c| c.source == Backup && c.is_selected == Some(false))
        .unwrap();
    let recovered = writer.read_snapshot_candidate(&old.id).unwrap();
    assert_eq!(recovered.source, Backup);
    assert_eq!(recovered.snapshot, a);
    assert_eq!(recovered.is_selected, Some(false));
    let held = recovered.snapshot.payload.unwrap();
    let token = writer.prepare_restore(&held).unwrap();
    let restored = writer.restore_snapshot(&held, &token).unwrap();
    assert_eq!(writer.load().unwrap(), restored);
    assert_eq!(restored.payload, Some(held));
    assert_eq!(family(&old_backup), original);
    assert_eq!(fs::read(&damaged_primary).unwrap(), b"damaged primary");
    assert_eq!(fs::read(&damaged_backup).unwrap(), b"damaged backup");
    assert_eq!(fs::read_to_string(&marker).unwrap(), new_stem);
}

#[test]
fn fixed_name_backup_remains_inspectable_after_rebuild_and_selector_damage() {
    use configuration_storage::SnapshotCandidateSource::Backup;

    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    let saved = writer.save(None, b"legacy bytes").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    checkpoint(&writer, generation).unwrap();
    let marker = root.path().join("active-backup");
    let directory = root.path().join("Backups");
    let original_id = fs::read_to_string(&marker).unwrap();
    let legacy = directory.join("snapshot.sqlite");
    fs::rename(directory.join(format!("{original_id}.sqlite")), &legacy).unwrap();
    fs::remove_file(&marker).unwrap();
    let original = family(&legacy);
    let page = writer.list_snapshot_candidates(64, None, None).unwrap();
    let reference = page.candidates.iter().find(|c| c.source == Backup).unwrap();
    assert_eq!(reference.is_selected, Some(true));
    let token = writer.prepare_backup_rebuild(generation).unwrap();
    writer.rebuild_backup(generation, &token).unwrap();
    assert_eq!(
        writer
            .read_snapshot_candidate(&reference.id)
            .unwrap()
            .is_selected,
        Some(false)
    );
    for marker_bytes in [None, Some(b"../untrusted".as_slice())] {
        match marker_bytes {
            Some(bytes) => fs::write(&marker, bytes).unwrap(),
            None => fs::remove_file(&marker).unwrap(),
        }
        let candidate = writer.read_snapshot_candidate(&reference.id).unwrap();
        assert_eq!(candidate.snapshot, saved);
        assert_eq!(candidate.is_selected, None);
        let page = writer.list_snapshot_candidates(64, None, None).unwrap();
        assert!(
            page.candidates
                .iter()
                .filter(|c| c.source == Backup)
                .all(|c| c.is_selected.is_none())
        );
        assert!(
            page.candidates
                .iter()
                .any(|c| c.source != Backup && c.is_selected == Some(true))
        );
    }
    assert_eq!(family(&legacy), original);
}

#[cfg(any(unix, windows))]
#[test]
fn unreadable_selector_is_advisory_only_for_candidate_inspection() {
    use configuration_storage::SnapshotCandidateSource::{Backup, Generation};

    for (marker_name, source) in [("active-backup", Backup), ("active-generation", Generation)] {
        let root = tempfile::tempdir().unwrap();
        let writer = store(root.path());
        let saved = writer.save(None, b"verified candidate").unwrap();
        checkpoint(&writer, saved.generation.as_deref().unwrap()).unwrap();
        let marker = root.path().join(marker_name);
        let bytes = fs::read(&marker).unwrap();

        #[cfg(unix)]
        let original_permissions = {
            use std::os::unix::fs::PermissionsExt;
            let original = fs::metadata(&marker).unwrap().permissions();
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap();
            original
        };
        #[cfg(windows)]
        let exclusive = {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&marker)
                .unwrap()
        };
        assert!(fs::read(&marker).is_err());

        let result = writer.list_snapshot_candidates(64, None, None);
        let candidate = result
            .as_ref()
            .ok()
            .and_then(|page| page.candidates.iter().find(|entry| entry.source == source))
            .map(|entry| writer.read_snapshot_candidate(&entry.id));
        let strict = if source == Backup {
            writer.read_backup().map(|_| ())
        } else {
            writer.prepare_restore(b"replacement").map(|_| ())
        };
        #[cfg(unix)]
        fs::set_permissions(&marker, original_permissions).unwrap();
        #[cfg(windows)]
        drop(exclusive);

        let page = result.unwrap();
        assert_eq!(page.candidates.len(), 2);
        assert_eq!(
            page.candidates
                .iter()
                .find(|entry| entry.source == source)
                .unwrap()
                .is_selected,
            None
        );
        assert_eq!(
            page.candidates
                .iter()
                .find(|entry| entry.source != source)
                .unwrap()
                .is_selected,
            Some(true)
        );
        let candidate = candidate.unwrap().unwrap();
        assert_eq!(candidate.is_selected, None);
        assert_eq!(candidate.snapshot, saved);
        assert_eq!(strict.unwrap_err().code, ErrorCode::IoFailure);
        assert_eq!(fs::read(marker).unwrap(), bytes);
    }
}

#[test]
fn scoped_catalog_pages_validate_cursors_and_isolate_other_directories() {
    use configuration_storage::SnapshotCandidateSource::{Backup, Generation};

    let root = tempfile::tempdir().unwrap();
    let writer = store(root.path());
    writer.save(None, b"verified generation").unwrap();
    let generations = root.path().join("Generations");
    let backups = root.path().join("Backups");
    fs::create_dir(&backups).unwrap();
    fs::write(backups.join("snapshot.sqlite"), b"candidate only").unwrap();
    fs::write(
        backups.join(format!("{}.sqlite", Uuid::new_v4())),
        b"candidate only",
    )
    .unwrap();
    for source in [Generation, Backup] {
        let first = writer
            .list_snapshot_candidates(1, None, Some(source))
            .unwrap();
        assert_eq!(first.candidates[0].source, source);
        let next = first.next_cursor.as_deref();
        if source == Backup {
            let next = next.unwrap();
            let second = writer
                .list_snapshot_candidates(1, Some(next), Some(source))
                .unwrap();
            assert_eq!(second.candidates[0].source, source);
            assert_ne!(second.candidates[0].id, next);
            assert!(second.next_cursor.is_none());
            assert_eq!(
                writer
                    .list_snapshot_candidates(1, Some(next), Some(Generation))
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidConfiguration
            );
        } else {
            assert!(next.is_none());
            assert_eq!(
                writer
                    .list_snapshot_candidates(1, Some(&first.candidates[0].id), Some(Backup))
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidConfiguration
            );
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let original = fs::metadata(&backups).unwrap().permissions();
        fs::set_permissions(&backups, fs::Permissions::from_mode(0o300)).unwrap();
        let all = writer.list_snapshot_candidates(64, None, None);
        let generation = writer.list_snapshot_candidates(64, None, Some(Generation));
        let backup = writer.list_snapshot_candidates(64, None, Some(Backup));
        fs::set_permissions(&backups, original).unwrap();
        assert_eq!(all.unwrap_err().code, ErrorCode::IoFailure);
        assert_eq!(backup.unwrap_err().code, ErrorCode::IoFailure);
        assert_eq!(generation.unwrap().candidates.len(), 1);
    }

    // A malformed managed directory must not masquerade as an empty catalog.
    for (directory, healthy, broken, count) in [
        (&backups, Generation, Backup, 1),
        (&generations, Backup, Generation, 2),
    ] {
        let retained = root.path().join("retained-directory");
        fs::rename(directory, &retained).unwrap();
        fs::write(directory, b"preserve this obstruction").unwrap();
        assert_eq!(
            writer
                .list_snapshot_candidates(64, None, Some(healthy))
                .unwrap()
                .candidates
                .len(),
            count
        );
        for source in [None, Some(broken)] {
            assert_eq!(
                writer
                    .list_snapshot_candidates(64, None, source)
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidStoredData
            );
        }
        assert_eq!(fs::read(directory).unwrap(), b"preserve this obstruction");
        fs::remove_file(directory).unwrap();
        fs::rename(retained, directory).unwrap();
    }
}

// Most fixtures need a reviewed healthy checkpoint; race tests pass captured tokens explicitly.
fn checkpoint(
    store: &ConfigurationStore,
    generation: &str,
) -> Result<configuration_storage::BackupSnapshot, configuration_storage::Failure> {
    let preparation = store.prepare_backup(generation)?;
    store.create_backup(generation, &preparation.token)
}
