use configuration_storage::{
    Configuration, ConfigurationStore, ErrorCode, MAXIMUM_PAYLOAD_BYTES, read_recovery_copy,
    validate_recovery_copy_configuration, write_recovery_copy,
};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

fn store(root: &Path) -> ConfigurationStore {
    ConfigurationStore::open(Configuration {
        directory: root.to_path_buf(),
        owner: "recovery.tests".into(),
        maximum_payload_bytes: 4096,
    })
    .unwrap()
}
fn selected(root: &Path) -> PathBuf {
    root.join("Generations").join(format!(
        "{}.sqlite",
        fs::read_to_string(root.join("active-generation")).unwrap()
    ))
}

fn family_member(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

fn family_bytes(database: &Path) -> [Option<Vec<u8>>; 4] {
    ["", "-journal", "-wal", "-shm"].map(|suffix| match fs::read(family_member(database, suffix)) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => panic!("Could not read {}{suffix}: {error}", database.display()),
    })
}

#[test]
fn first_save_retries_after_abandoned_private_staging() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let writer = store(&root);
    let stages = root.join("Generations");
    let empty = stages.join(".generation-AbC123");
    let incomplete = stages.join(".generation-DeF456");
    fs::create_dir_all(&empty).unwrap();
    fs::create_dir(&incomplete).unwrap();
    let database = incomplete.join(".tmpGhI789");
    fs::write(&database, b"interrupted database bytes").unwrap();
    fs::write(family_member(&database, "-journal"), b"unfinished journal").unwrap();
    let evidence = family_bytes(&database);

    let before = writer.load().unwrap();
    assert_eq!(before.generation, None);
    assert_eq!(before.payload, None);
    assert!(
        writer
            .list_snapshot_candidates(64, None, None)
            .unwrap()
            .candidates
            .is_empty()
    );
    let saved = writer.save(None, b"first accepted write").unwrap();

    assert_eq!(store(&root).load().unwrap(), saved);
    assert_eq!(
        saved.payload.as_deref(),
        Some(b"first accepted write".as_slice())
    );
    assert_eq!(family_bytes(&database), evidence);
    assert_eq!(fs::read_dir(empty).unwrap().count(), 0);
}

#[test]
fn first_save_rejects_entries_outside_the_private_staging_layout() {
    let cases: &[&[&str]] = &[
        &["arbitrary-file"],
        &[".generation-AbC123"],
        &[".generation-not-legacy/.tmpGhI789"],
        &[".generation-AbC123/.tmpGhI789-journal"],
        &[
            ".generation-AbC123/.tmpGhI789",
            ".generation-AbC123/unrelated",
        ],
        &[
            ".generation-AbC123/.tmpGhI789",
            ".generation-AbC123/.tmpJkL084",
        ],
        &[
            ".generation-AbC123/.tmpGhI789",
            ".generation-AbC123/.tmpJkL084-wal",
        ],
        &[".generation-AbC123/.tmpGhI789/unexpected-child"],
    ];
    for entries in cases {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("store");
        let writer = store(&root);
        let generations = root.join("Generations");
        for entry in *entries {
            let path = generations.join(entry);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"retained evidence").unwrap();
        }

        assert_eq!(
            writer.load().unwrap_err().code,
            ErrorCode::InvalidStoredData,
            "{entries:?}"
        );
        assert_eq!(
            writer.save(None, b"seed").unwrap_err().code,
            ErrorCode::InvalidStoredData,
            "{entries:?}"
        );
        assert!(!root.join("active-generation").exists());
        for entry in *entries {
            assert_eq!(
                fs::read(generations.join(entry)).unwrap(),
                b"retained evidence"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn first_save_rejects_links_in_private_staging() {
    use std::os::unix::fs::symlink;

    for link_directory in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("store");
        let writer = store(&root);
        let outside = directory.path().join("unrelated");
        fs::create_dir(&outside).unwrap();
        let evidence = outside.join(".tmpGhI789");
        fs::write(&evidence, b"outside bytes").unwrap();
        let stage = root.join("Generations/.generation-AbC123");
        fs::create_dir(stage.parent().unwrap()).unwrap();
        if link_directory {
            symlink(&outside, &stage).unwrap();
        } else {
            fs::create_dir(&stage).unwrap();
            symlink(&evidence, stage.join(".tmpGhI789")).unwrap();
        }

        assert_eq!(
            writer.load().unwrap_err().code,
            ErrorCode::InvalidStoredData
        );
        assert_eq!(
            writer.save(None, b"seed").unwrap_err().code,
            ErrorCode::InvalidStoredData
        );
        assert!(!root.join("active-generation").exists());
        assert_eq!(fs::read(evidence).unwrap(), b"outside bytes");
    }
}

#[test]
fn first_save_requires_explicit_recovery_once_the_generation_is_published() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let writer = store(&root);
    let saved = writer.save(None, b"published candidate").unwrap();
    let database = selected(&root);
    let evidence = family_bytes(&database);
    fs::remove_file(root.join("active-generation")).unwrap();
    fs::create_dir(root.join("Generations/.generation-AbC123")).unwrap();

    assert_eq!(
        writer.load().unwrap_err().code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(
        writer.save(None, b"seed").unwrap_err().code,
        ErrorCode::InvalidStoredData
    );
    let page = writer.list_snapshot_candidates(64, None, None).unwrap();
    let [reference] = &page.candidates[..] else {
        panic!("Expected the published candidate to remain available for explicit recovery");
    };
    assert_eq!(reference.is_selected, None);
    let candidate = writer.read_snapshot_candidate(&reference.id).unwrap();
    assert_eq!(candidate.snapshot, saved);
    let payload = candidate.snapshot.payload.unwrap();
    let token = writer.prepare_restore(&payload).unwrap();
    let restored = writer.restore_snapshot(&payload, &token).unwrap();
    assert_eq!(restored.payload, Some(payload));
    assert_eq!(family_bytes(&database), evidence);
}

#[test]
fn recovery_configuration_admission_never_probes_or_creates_paths() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-parent/copy");
    for maximum in [1, MAXIMUM_PAYLOAD_BYTES] {
        validate_recovery_copy_configuration(&missing, maximum).unwrap();
    }
    for (path, maximum) in [
        (missing.as_path(), 0),
        (missing.as_path(), MAXIMUM_PAYLOAD_BYTES + 1),
        (Path::new("relative.copy"), 1),
        (directory.path().join("../copy").as_path(), 1),
    ] {
        assert_eq!(
            validate_recovery_copy_configuration(path, maximum)
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfiguration
        );
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn nul_paths_do_not_select_the_file_named_by_their_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let original_path = directory.path().join("copy");
    write_recovery_copy(&original_path, b"original", 32).unwrap();
    let original = fs::read(&original_path).unwrap();
    let malformed = directory.path().join("copy\0suffix");
    assert_eq!(
        [
            validate_recovery_copy_configuration(&malformed, 32)
                .err()
                .map(|error| error.code),
            read_recovery_copy(&malformed, 32)
                .err()
                .map(|error| error.code),
            write_recovery_copy(&malformed, b"replacement", 32)
                .err()
                .map(|error| error.code)
        ],
        [Some(ErrorCode::InvalidConfiguration); 3]
    );
    assert_eq!(fs::read(original_path).unwrap(), original);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn native_admission_does_not_require_utf8_unix_paths() {
    use std::os::unix::ffi::OsStrExt;

    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join(std::ffi::OsStr::from_bytes(b"copy\xff"));
    validate_recovery_copy_configuration(&path, 1).unwrap();
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[cfg(windows)]
#[test]
fn native_admission_does_not_require_unicode_windows_paths() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(OsString::from_wide(&[0xd800]));
    validate_recovery_copy_configuration(&path, 1).unwrap();
}

#[test]
fn valid_copies_keep_signed_fractional_file_dates_without_changing_bytes() {
    use std::time::{Duration, UNIX_EPOCH};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    write_recovery_copy(&path, b"accepted portable bytes", 100).unwrap();
    let original = fs::read(&path).unwrap();
    let before = read_recovery_copy(&path, 100).unwrap();
    for (date, seconds) in [
        (UNIX_EPOCH - Duration::from_millis(1250), -1.25),
        (UNIX_EPOCH - Duration::from_nanos(100), -1e-7),
        (UNIX_EPOCH, 0.0),
        (UNIX_EPOCH + Duration::from_nanos(200), 2e-7),
        (UNIX_EPOCH + Duration::from_millis(1250), 1.25),
    ] {
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(date)
            .unwrap();
        let read = read_recovery_copy(&path, 100).unwrap();
        assert_eq!(read.modified_at, seconds);
        assert_eq!(read.payload, before.payload);
        assert_eq!(read.digest, before.digest);
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[test]
fn independent_copy_round_trips_binary_and_empty_bytes_without_any_store() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("復旧 копия.recovery");
    let moved = directory.path().join("reviewed.copy");
    let payload: Vec<_> = (0..=255).collect();
    write_recovery_copy(&path, &payload, 256).unwrap();
    let copy = read_recovery_copy(&path, 256).unwrap();
    assert_eq!(copy.payload, payload);
    assert_eq!(copy.digest.len(), 64);
    assert!(copy.modified_at > 0.0);
    fs::rename(&path, &moved).unwrap();
    assert_eq!(read_recovery_copy(&moved, 256).unwrap(), copy);
    write_recovery_copy(&moved, &[], 256).unwrap();
    let empty = read_recovery_copy(&moved, 256).unwrap();
    assert!(empty.payload.is_empty());
    assert_ne!(empty.digest, copy.digest);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(moved).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn malformed_truncated_modified_and_appended_copies_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    write_recovery_copy(&path, b"complete bytes", 100).unwrap();
    let original = fs::read(&path).unwrap();
    let rejects_without_writing = |bytes: &[u8], error| {
        fs::write(&path, bytes).unwrap();
        assert_eq!(read_recovery_copy(&path, 100).unwrap_err().code, error);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    };
    for count in [0, 7, 19, 51, original.len() - 1] {
        rejects_without_writing(&original[..count], ErrorCode::InvalidRecoveryCopy);
    }
    for offset in [0, 12, 20, original.len() - 1] {
        let mut modified = original.clone();
        modified[offset] ^= 1;
        rejects_without_writing(&modified, ErrorCode::InvalidRecoveryCopy);
    }
    let mut appended = original.clone();
    appended.push(0);
    rejects_without_writing(&appended, ErrorCode::InvalidRecoveryCopy);
    let mut unsupported = original;
    unsupported[8..12].copy_from_slice(&2_u32.to_le_bytes());
    rejects_without_writing(&unsupported, ErrorCode::UnsupportedRecoveryVersion);
}

#[test]
fn resource_limits_and_invalid_locations_do_not_replace_existing_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    write_recovery_copy(&path, b"old", 3).unwrap();
    let original = fs::read(&path).unwrap();
    assert_eq!(
        write_recovery_copy(&path, b"longer", 3).unwrap_err().code,
        ErrorCode::InvalidConfiguration
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(
        read_recovery_copy(&path, 2).unwrap_err().code,
        ErrorCode::InvalidRecoveryCopy
    );
    for limit in [0, MAXIMUM_PAYLOAD_BYTES + 1] {
        assert_eq!(
            read_recovery_copy(&path, limit).unwrap_err().code,
            ErrorCode::InvalidConfiguration
        );
    }
    assert_eq!(
        write_recovery_copy(Path::new("relative.copy"), b"a", 1)
            .unwrap_err()
            .code,
        ErrorCode::InvalidConfiguration
    );
    let missing = directory.path().join("missing/copy");
    assert!(write_recovery_copy(&missing, b"a", 1).is_err());
    assert!(!missing.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn final_links_are_rejected_and_targets_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original");
    write_recovery_copy(&original, b"protected", 100).unwrap();
    let bytes = fs::read(&original).unwrap();
    let alias = directory.path().join("alias");
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    assert!(write_recovery_copy(&alias, b"new", 100).is_err());
    assert!(read_recovery_copy(&alias, 100).is_err());
    assert_eq!(fs::read_link(alias).unwrap(), original);
    assert_eq!(fs::read(original).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn recovery_copy_resolves_parent_alias_without_following_the_final_file() {
    let directory = tempfile::tempdir().unwrap();
    let real = directory.path().join("real");
    fs::create_dir(&real).unwrap();
    let alias = directory.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    let physical_copy = real.join("copy");
    let selected = alias.join("copy");
    write_recovery_copy(&selected, b"first", 100).unwrap();
    assert_eq!(
        read_recovery_copy(&physical_copy, 100)
            .unwrap()
            .payload
            .as_slice(),
        b"first"
    );
    write_recovery_copy(&selected, b"second", 100).unwrap();
    assert_eq!(
        read_recovery_copy(&selected, 100)
            .unwrap()
            .payload
            .as_slice(),
        b"second"
    );
    let original = fs::read(&physical_copy).unwrap();

    let final_link = alias.join("final-link");
    std::os::unix::fs::symlink(&physical_copy, &final_link).unwrap();
    assert_eq!(
        write_recovery_copy(&final_link, b"do not write", 100)
            .unwrap_err()
            .code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(fs::read_link(&final_link).unwrap(), physical_copy);
    assert_eq!(fs::read(physical_copy).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn recovery_copy_does_not_normalize_directory_spellings_into_file_writes() {
    let directory = tempfile::tempdir().unwrap();
    let existing = directory.path().join("existing");
    let missing = directory.path().join("missing");
    write_recovery_copy(&existing, b"keep", 100).unwrap();
    let original = fs::read(&existing).unwrap();

    for path in [&existing, &missing] {
        for suffix in ["/", "/."] {
            let mut spelling = path.as_os_str().to_os_string();
            spelling.push(suffix);
            assert_eq!(
                write_recovery_copy(Path::new(&spelling), b"do not write", 100)
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidConfiguration
            );
        }
    }
    assert_eq!(fs::read(existing).unwrap(), original);
    assert!(!missing.exists());
}

#[test]
fn preparing_is_observation_and_restoration_retains_the_original_generation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let writer = store(&root);
    let before = writer.save(None, b"original").unwrap();
    let primary = selected(&root);
    let original = fs::read(&primary).unwrap();
    let token = writer.prepare_restore(b"reviewed").unwrap();
    assert_eq!(writer.load().unwrap(), before);
    assert_eq!(fs::read(&primary).unwrap(), original);
    let restored = writer.restore_snapshot(b"reviewed", &token).unwrap();
    assert_eq!(restored.payload.as_deref(), Some(b"reviewed".as_slice()));
    assert_ne!(restored.generation, before.generation);
    assert_ne!(selected(&root), primary);
    assert_eq!(fs::read(&primary).unwrap(), original);
    assert_eq!(
        writer
            .save(before.generation.as_deref(), b"stale edit")
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        writer
            .restore_snapshot(b"reviewed", &token)
            .unwrap_err()
            .code,
        ErrorCode::RestoreChanged
    );
}

#[test]
fn changed_payload_leaves_selection_untouched_and_the_reviewed_bytes_restorable() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let writer = store(&root);
    let initial = writer.save(None, b"initial").unwrap();
    let approved: Vec<u8> = (0..=u8::MAX).collect();
    let token = writer.prepare_restore(&approved).unwrap();
    let selector = root.join("active-generation");
    let selection = fs::read(&selector).unwrap();
    let primary = selected(&root);
    let before = family_bytes(&primary);
    let mut changed = approved.clone();
    changed[0] ^= 1;

    assert_eq!(
        writer.restore_snapshot(&changed, &token).unwrap_err().code,
        ErrorCode::RestoreChanged
    );
    assert_eq!(fs::read(&selector).unwrap(), selection);
    assert_eq!(family_bytes(&primary), before);
    assert_eq!(fs::read_dir(root.join("Generations")).unwrap().count(), 1);
    assert_eq!(writer.load().unwrap(), initial);

    let restored = writer.restore_snapshot(&approved, &token).unwrap();
    assert_eq!(restored.payload, Some(approved));
    assert!(restored.generation.is_some());
    assert_ne!(restored.generation, initial.generation);
    assert_eq!(family_bytes(&primary), before);
}

#[test]
fn an_intervening_write_requires_review_and_leaves_old_writer_generations_stale() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let writer = store(&root);
    let initial = writer.save(None, b"initial").unwrap();
    let approved = b"reviewed import";
    let token = writer.prepare_restore(approved).unwrap();
    let external = store(&root)
        .save(initial.generation.as_deref(), b"external")
        .unwrap();
    let selector = root.join("active-generation");
    let selection = fs::read(&selector).unwrap();
    let primary = selected(&root);
    let before = family_bytes(&primary);

    assert_eq!(
        writer.restore_snapshot(approved, &token).unwrap_err().code,
        ErrorCode::RestoreChanged
    );
    assert_eq!(fs::read(&selector).unwrap(), selection);
    assert_eq!(family_bytes(&primary), before);
    assert_eq!(writer.load().unwrap(), external);
    let refreshed = writer.prepare_restore(approved).unwrap();
    assert_ne!(refreshed, token);
    assert_eq!(
        writer
            .save(initial.generation.as_deref(), b"stale edit")
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let restored = writer.restore_snapshot(approved, &refreshed).unwrap();
    assert_eq!(restored.payload.as_deref(), Some(approved.as_slice()));
    assert_ne!(restored.generation, initial.generation);
    assert_ne!(restored.generation, external.generation);
    assert_ne!(selected(&root), primary);
    assert_eq!(family_bytes(&primary), before);
    assert_eq!(
        writer
            .save(external.generation.as_deref(), b"another stale edit")
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(store(&root).load().unwrap(), restored);
}

#[test]
fn raw_damage_after_review_is_not_accepted_by_the_old_token() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let writer = store(&root);
    writer.save(None, b"initial").unwrap();
    let primary = selected(&root);
    let token = writer.prepare_restore(b"recovered").unwrap();
    let damaged = b"unreadable original";
    fs::write(&primary, damaged).unwrap();
    assert_eq!(
        writer
            .restore_snapshot(b"recovered", &token)
            .unwrap_err()
            .code,
        ErrorCode::RestoreChanged
    );
    assert_eq!(fs::read(&primary).unwrap(), damaged);
    let reviewed = writer.prepare_restore(b"recovered").unwrap();
    writer.restore_snapshot(b"recovered", &reviewed).unwrap();
    assert_eq!(fs::read(primary).unwrap(), damaged);
    assert_eq!(
        writer.load().unwrap().payload.as_deref(),
        Some(b"recovered".as_slice())
    );
}

#[test]
fn damaged_selection_journal_changes_invalidate_review_without_touching_the_family() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let writer = store(&root);
    let initial = writer.save(None, b"initial").unwrap();
    let selector = root.join("active-generation");
    let selection = fs::read(&selector).unwrap();
    let primary = selected(&root);
    let journal = family_member(&primary, "-journal");
    let damaged = [0, 1, 2, 255, 0];
    fs::write(&primary, damaged).unwrap();
    assert_eq!(
        writer.load().unwrap_err().code,
        ErrorCode::InvalidStoredData
    );
    // Loading can ask SQLite to inspect its journal. Restore review must read raw bytes.
    let reviewed_journal = [5, 4, 3, 255];
    fs::write(&journal, reviewed_journal).unwrap();
    let reviewed_family = family_bytes(&primary);
    assert_eq!(reviewed_family[0].as_deref(), Some(damaged.as_slice()));
    assert_eq!(
        reviewed_family[1].as_deref(),
        Some(reviewed_journal.as_slice())
    );
    let approved = [0, 255, 128, 42];
    let token = writer.prepare_restore(&approved).unwrap();
    assert_eq!(fs::read(&selector).unwrap(), selection);
    assert_eq!(family_bytes(&primary), reviewed_family);

    let changed_journal = [4, 4, 3, 255];
    fs::write(&journal, changed_journal).unwrap();
    let mut changed_family = reviewed_family;
    changed_family[1] = Some(changed_journal.to_vec());
    let generations = root.join("Generations");
    let original_entries = fs::read_dir(&generations).unwrap().count();
    assert_eq!(
        writer.restore_snapshot(&approved, &token).unwrap_err().code,
        ErrorCode::RestoreChanged
    );
    assert_eq!(fs::read(&selector).unwrap(), selection);
    assert_eq!(family_bytes(&primary), changed_family);
    assert_eq!(fs::read_dir(generations).unwrap().count(), original_entries);

    let refreshed = writer.prepare_restore(&approved).unwrap();
    assert_ne!(refreshed, token);
    let restored = writer.restore_snapshot(&approved, &refreshed).unwrap();
    assert_eq!(restored.payload.as_deref(), Some(approved.as_slice()));
    assert!(restored.generation.is_some());
    assert_ne!(restored.generation, initial.generation);
    assert_ne!(selected(&root), primary);
    assert_eq!(family_bytes(&primary), changed_family);
    assert_eq!(store(&root).load().unwrap(), restored);
    assert_eq!(
        writer
            .save(initial.generation.as_deref(), b"stale edit")
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[test]
fn restore_payload_limit_is_checked_before_acquiring_the_root_lock() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let approved = b"approved";
    let writer = ConfigurationStore::open(Configuration {
        directory: root.clone(),
        owner: "recovery.tests".into(),
        maximum_payload_bytes: approved.len(),
    })
    .unwrap();
    let initial = writer.save(None, b"initial").unwrap();
    let token = writer.prepare_restore(approved).unwrap();
    let selector = root.join("active-generation");
    let original_selector = fs::read(&selector).unwrap();
    let primary = selected(&root);
    let original_primary = fs::read(&primary).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("store.lock"))
        .unwrap();
    lock.lock().unwrap();

    for (payload, expected) in [
        (b"oversized".as_slice(), ErrorCode::InvalidConfiguration),
        (approved.as_slice(), ErrorCode::Busy),
    ] {
        assert_eq!(
            (
                writer.prepare_restore(payload).unwrap_err().code,
                writer.restore_snapshot(payload, &token).unwrap_err().code,
            ),
            (expected, expected)
        );
    }
    drop(lock);
    assert_eq!(fs::read(&selector).unwrap(), original_selector);
    assert_eq!(fs::read(&primary).unwrap(), original_primary);
    assert!(!root.join("Recovery").exists());
    assert_eq!(writer.load().unwrap(), initial);

    let restored = writer.restore_snapshot(approved, &token).unwrap();
    assert_eq!(restored.payload.as_deref(), Some(approved.as_slice()));
    assert_eq!(fs::read(primary).unwrap(), original_primary);
}

#[test]
fn uninitialized_restore_creates_one_selected_generation_and_bounds_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let writer = store(&root);
    assert_eq!(
        writer.prepare_restore(&vec![0; 4097]).unwrap_err().code,
        ErrorCode::InvalidConfiguration
    );
    assert!(!root.join("Generations").exists());
    let token = writer.prepare_restore(&[]).unwrap();
    assert!(writer.load().unwrap().generation.is_none());
    writer.restore_snapshot(&[], &token).unwrap();
    assert_eq!(writer.load().unwrap().payload, Some(vec![]));
    assert_eq!(fs::read_dir(root.join("Generations")).unwrap().count(), 1);
}
