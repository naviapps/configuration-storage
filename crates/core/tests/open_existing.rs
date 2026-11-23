use configuration_storage::{Configuration, ConfigurationStore, ErrorCode, Failure, Snapshot};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

const OWNER: &str = "com.example.fixture";

fn config(directory: &Path) -> Configuration {
    Configuration {
        directory: directory.to_owned(),
        owner: OWNER.into(),
        maximum_payload_bytes: 1024,
    }
}

fn failure<T>(result: Result<T, Failure>) -> Failure {
    result.err().expect("operation must fail")
}

fn entries(directory: &Path) -> Vec<OsString> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

fn fixture() -> tempfile::TempDir {
    tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

fn selected_database(directory: &Path) -> PathBuf {
    let name = fs::read_to_string(directory.join("active-generation")).unwrap();
    directory.join("Generations").join(format!("{name}.sqlite"))
}

#[cfg(windows)]
fn junction(directory: &Path, name: &'static str, target: &'static str) {
    // Only fixed fixture names enter cmd; the temporary path is the process cwd.
    let result = std::process::Command::new("cmd.exe")
        .current_dir(directory)
        .args(["/d", "/c", "mklink", "/J", name, target])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "cannot prepare owned junction fixture: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[cfg(windows)]
fn only_sqlite_file(directory: &Path) -> std::path::PathBuf {
    let mut files = entries(directory)
        .into_iter()
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "sqlite")
        })
        .map(|name| directory.join(name));
    let database = files.next().expect("fixture must have a SQLite generation");
    assert!(
        files.next().is_none(),
        "fixture must have only one SQLite generation"
    );
    assert!(fs::symlink_metadata(&database).unwrap().is_file());
    database
}

#[test]
fn only_an_absent_root_is_missing_and_rejected_roots_are_unchanged() {
    let parent = fixture();
    let directory = parent.path().join("nested/store");
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(&directory))),
        Failure::new(ErrorCode::StoreMissing)
    );
    assert!(entries(parent.path()).is_empty());
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(Path::new(
            "relative/store"
        ))))
        .code,
        ErrorCode::InvalidConfiguration
    );

    fs::create_dir_all(&directory).unwrap();
    for unclaimed in [None, Some("unrelated-file")] {
        if let Some(name) = unclaimed {
            fs::write(directory.join(name), b"unclaimed bytes").unwrap();
        }
        let before = entries(&directory);
        assert_eq!(
            failure(ConfigurationStore::open_existing(config(&directory))).code,
            ErrorCode::OwnerMismatch
        );
        assert_eq!(entries(&directory), before);
        if let Some(name) = unclaimed {
            assert_eq!(fs::read(directory.join(name)).unwrap(), b"unclaimed bytes");
        }
    }
    fs::write(directory.join("store-owner"), b"com.example.foreign").unwrap();
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(&directory))).code,
        ErrorCode::OwnerMismatch
    );
    assert!(!directory.join("store.lock").exists());
    assert_eq!(
        fs::read(directory.join("store-owner")).unwrap(),
        b"com.example.foreign"
    );
    assert_eq!(
        fs::read(directory.join("unrelated-file")).unwrap(),
        b"unclaimed bytes"
    );
    let file = directory.join("unrelated-file");
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(&file))).code,
        ErrorCode::InvalidStoredData
    );
    assert_ne!(
        failure(ConfigurationStore::open_existing(config(
            &file.join("store")
        )))
        .code,
        ErrorCode::StoreMissing
    );
}

#[test]
fn malformed_ownership_and_missing_or_invalid_locks_are_not_repaired() {
    let parent = fixture();
    let directory = parent.path();
    let marker = directory.join("store-owner");
    let lock = directory.join("store.lock");

    fs::create_dir(&marker).unwrap();
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(directory))).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(entries(directory), [OsString::from("store-owner")]);
    assert!(entries(&marker).is_empty());
    fs::remove_dir(&marker).unwrap();

    for bytes in [&b""[..], b"com.example\nfixture", b"\xff", &[b'a'; 1025]] {
        fs::write(&marker, bytes).unwrap();
        assert_eq!(
            failure(ConfigurationStore::open_existing(config(directory))).code,
            ErrorCode::InvalidStoredData
        );
        assert_eq!(fs::read(&marker).unwrap(), bytes);
        assert!(!lock.exists());
    }
    fs::write(&marker, OWNER).unwrap();
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(directory))).code,
        ErrorCode::InvalidStoredData
    );
    assert!(!lock.exists());

    fs::create_dir(&lock).unwrap();
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(directory))).code,
        ErrorCode::InvalidStoredData
    );
    assert!(lock.is_dir());
    assert!(entries(&lock).is_empty());
}

#[test]
fn existing_admission_distinguishes_owned_empty_from_initialized_and_requires_the_same_lock_policy()
{
    let parent = fixture();
    let directory = parent.path().join("store");
    drop(ConfigurationStore::open(config(&directory)).unwrap());
    let lock_path = directory.join("store.lock");
    fs::write(&lock_path, b"lock sentinel").unwrap();
    let before = entries(&directory);

    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    lock.try_lock().unwrap();
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(&directory))).code,
        ErrorCode::Busy
    );
    drop(lock);

    let existing = ConfigurationStore::open_existing(config(&directory)).unwrap();
    assert_eq!(entries(&directory), before);
    assert_eq!(
        existing.load().unwrap(),
        Snapshot {
            generation: None,
            payload: None,
        }
    );
    let saved = existing.save(None, b"owned contents").unwrap();
    let reopened = ConfigurationStore::open_existing(config(&directory)).unwrap();
    assert_eq!(reopened.load().unwrap(), saved);
    assert_eq!(fs::read(&lock_path).unwrap(), b"lock sentinel");

    fs::remove_file(&lock_path).unwrap();
    assert_eq!(failure(reopened.load()).code, ErrorCode::InvalidStoredData);
    assert_eq!(
        failure(ConfigurationStore::open_existing(config(&directory))).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(
        failure(reopened.list_snapshot_candidates(1, None, None)).code,
        ErrorCode::InvalidStoredData
    );
    assert!(!lock_path.exists());
    assert_eq!(
        fs::read(directory.join("store-owner")).unwrap(),
        OWNER.as_bytes()
    );
}

#[test]
fn foreign_admission_preserves_the_selected_store_and_opaque_bytes() {
    let parent = fixture();
    let directory = parent.path().join("store");
    let writer = ConfigurationStore::open(config(&directory)).unwrap();
    let payload = &[0, 255, 128, 0];
    let saved = writer.save(None, payload).unwrap();
    let paths = [
        directory.join("store-owner"),
        directory.join("store.lock"),
        directory.join("active-generation"),
        selected_database(&directory),
    ];
    let files = paths.map(|path| {
        let bytes = fs::read(&path).unwrap();
        (path, bytes)
    });
    let before = entries(&directory);
    let foreign = Configuration {
        owner: "com.example.foreign".into(),
        ..config(&directory)
    };

    assert_eq!(
        failure(ConfigurationStore::open_existing(foreign)).code,
        ErrorCode::OwnerMismatch
    );
    assert_eq!(entries(&directory), before);
    for (path, bytes) in files {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    let reopened = ConfigurationStore::open_existing(config(&directory)).unwrap();
    let loaded = reopened.load().unwrap();
    assert_eq!(loaded, saved);
    assert_eq!(loaded.payload.as_deref(), Some(payload.as_slice()));
}

#[test]
fn malformed_selection_is_rejected_on_load_without_replacing_saved_bytes() {
    for malformed in [&[0, 255][..], &[255, 0, 47], b"not-a-generation"] {
        let parent = fixture();
        let directory = parent.path().join("store");
        let writer = ConfigurationStore::open(config(&directory)).unwrap();
        writer.save(None, &[0, 255, 42]).unwrap();
        let database = selected_database(&directory);
        let original = fs::read(&database).unwrap();
        let selector = directory.join("active-generation");
        fs::write(&selector, malformed).unwrap();
        let before = entries(&directory);
        assert!(!directory.join("Recovery").exists());

        let existing = ConfigurationStore::open_existing(config(&directory)).unwrap();
        assert_eq!(entries(&directory), before);
        assert_eq!(fs::read(&selector).unwrap(), malformed);
        assert_eq!(fs::read(&database).unwrap(), original);

        assert_eq!(failure(existing.load()).code, ErrorCode::InvalidStoredData);
        assert_eq!(
            failure(existing.save(None, b"unexpected default")).code,
            ErrorCode::InvalidStoredData
        );
        assert_eq!(fs::read(&selector).unwrap(), malformed);
        assert_eq!(fs::read(&database).unwrap(), original);
        assert_eq!(
            entries(&directory.join("Generations")),
            [database.file_name().unwrap().to_os_string()]
        );
    }
}

#[test]
fn admission_does_not_open_sqlite_and_corruption_is_not_an_empty_store() {
    let parent = fixture();
    let directory = parent.path().join("store");
    let writer = ConfigurationStore::open(config(&directory)).unwrap();
    let saved = writer.save(None, b"prior accepted contents").unwrap();
    checkpoint(&writer, saved.generation.as_deref().unwrap()).unwrap();
    drop(writer);

    let selector = directory.join("active-generation");
    let name = fs::read_to_string(&selector).unwrap();
    let database = directory.join("Generations").join(format!("{name}.sqlite"));
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE store_state SET payload=?1",
            [b"wrong digest".as_slice()],
        )
        .unwrap();
    connection.close().unwrap();
    let corrupt = fs::read(&database).unwrap();
    let original_entries = entries(&directory);

    let store = ConfigurationStore::open_existing(config(&directory)).unwrap();
    assert_eq!(entries(&directory), original_entries);
    assert_eq!(fs::read(&database).unwrap(), corrupt);
    let error = failure(store.load());
    assert_eq!(error.code, ErrorCode::InvalidStoredData);
    let captured = error.recovery_directory.unwrap();
    assert_eq!(
        fs::read(captured.join(database.file_name().unwrap())).unwrap(),
        corrupt
    );
    assert_eq!(
        failure(store.save(None, b"default")).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(fs::read(database).unwrap(), corrupt);
    assert_eq!(fs::read_to_string(selector).unwrap(), name);
    assert_eq!(store.read_backup().unwrap().snapshot, saved);
}

#[cfg(unix)]
#[test]
fn existing_admission_resolves_parent_aliases_but_rejects_links_at_managed_leaves() {
    use std::os::unix::fs::symlink;

    let parent = fixture();
    let directory = parent.path().join("store");
    let store = ConfigurationStore::open(config(&directory)).unwrap();
    let saved = store.save(None, b"owned contents").unwrap();
    let alias = parent.path().join("parent-alias");
    symlink(parent.path(), &alias).unwrap();
    let through_parent = ConfigurationStore::open_existing(config(&alias.join("store"))).unwrap();
    assert_eq!(through_parent.load().unwrap(), saved);

    let root_alias = parent.path().join("root-alias");
    symlink(&directory, &root_alias).unwrap();
    let dangling = parent.path().join("dangling");
    symlink(parent.path().join("absent"), &dangling).unwrap();
    for path in [root_alias.join("."), dangling] {
        assert_eq!(
            failure(ConfigurationStore::open_existing(config(&path))).code,
            ErrorCode::InvalidStoredData
        );
    }
    for name in ["store-owner", "store.lock"] {
        let path = directory.join(name);
        let original = parent.path().join(name);
        fs::rename(&path, &original).unwrap();
        let bytes = fs::read(&original).unwrap();
        symlink(&original, &path).unwrap();
        assert_eq!(
            failure(ConfigurationStore::open_existing(config(&directory))).code,
            ErrorCode::InvalidStoredData
        );
        assert_eq!(fs::read_link(&path).unwrap(), original);
        assert_eq!(fs::read(&original).unwrap(), bytes);
        fs::remove_file(&path).unwrap();
        fs::rename(original, path).unwrap();
    }
}

#[cfg(windows)]
#[test]
fn existing_admission_resolves_parent_junctions_but_rejects_junction_roots() {
    let parent = fixture();
    let real = parent.path().join("real");
    fs::create_dir(&real).unwrap();
    let directory = real.join("store");
    let store = ConfigurationStore::open(config(&directory)).unwrap();
    let saved = store.save(None, b"owned contents").unwrap();
    let database = only_sqlite_file(&directory.join("Generations"));
    let bytes = fs::read(&database).unwrap();
    let original_entries = entries(&directory);
    let markers = ["store-owner", "store.lock", "active-generation"];
    let original_markers = markers.map(|name| fs::read(directory.join(name)).unwrap());

    junction(parent.path(), "parent-alias", "real");
    let through_parent =
        ConfigurationStore::open_existing(config(&parent.path().join("parent-alias/store")))
            .unwrap();
    assert_eq!(
        through_parent.configuration().directory,
        fs::canonicalize(&directory).unwrap()
    );
    assert_eq!(through_parent.load().unwrap(), saved);

    junction(parent.path(), "root-alias", "real\\store");
    let root_alias = parent.path().join("root-alias");
    let root_target = fs::read_link(&root_alias).unwrap();
    fs::create_dir(parent.path().join("removed")).unwrap();
    junction(parent.path(), "dangling", "removed");
    let dangling = parent.path().join("dangling");
    let dangling_target = fs::read_link(&dangling).unwrap();
    fs::remove_dir(parent.path().join("removed")).unwrap();
    assert!(!dangling.exists());
    assert!(fs::symlink_metadata(&dangling).unwrap().is_symlink());

    for path in [&root_alias, &root_alias.join("."), &dangling] {
        assert_eq!(
            failure(ConfigurationStore::open_existing(config(path))).code,
            ErrorCode::InvalidStoredData
        );
    }
    assert_eq!(fs::read_link(root_alias).unwrap(), root_target);
    assert_eq!(fs::read_link(dangling).unwrap(), dangling_target);
    assert_eq!(entries(&directory), original_entries);
    assert_eq!(
        markers.map(|name| fs::read(directory.join(name)).unwrap()),
        original_markers
    );
    assert_eq!(fs::read(database).unwrap(), bytes);
}

#[cfg(windows)]
#[test]
fn junctioned_generations_are_rejected_without_changing_the_target() {
    let parent = fixture();
    let directory = parent.path().join("store");
    let store = ConfigurationStore::open(config(&directory)).unwrap();
    let saved = store.save(None, b"owned contents").unwrap();
    let generations = directory.join("Generations");
    let retained = parent.path().join("retained-generations");
    fs::rename(&generations, &retained).unwrap();
    let database = only_sqlite_file(&retained);
    let bytes = fs::read(&database).unwrap();
    let retained_entries = entries(&retained);
    let selector = fs::read(directory.join("active-generation")).unwrap();
    junction(&directory, "Generations", "..\\retained-generations");
    let target = fs::read_link(&generations).unwrap();
    let original_entries = entries(&directory);
    let parent_entries = entries(parent.path());

    assert_eq!(failure(store.load()).code, ErrorCode::InvalidStoredData);
    assert_eq!(
        failure(store.save(saved.generation.as_deref(), b"must not write")).code,
        ErrorCode::InvalidStoredData
    );
    assert_eq!(entries(&directory), original_entries);
    assert_eq!(entries(parent.path()), parent_entries);
    assert_eq!(entries(&retained), retained_entries);
    assert_eq!(fs::read(database).unwrap(), bytes);
    assert_eq!(
        fs::read(directory.join("active-generation")).unwrap(),
        selector
    );
    assert_eq!(fs::read_link(&generations).unwrap(), target);
    assert!(!directory.join("Recovery").exists());

    fs::remove_dir(&generations).unwrap();
    fs::rename(retained, generations).unwrap();
    assert_eq!(store.load().unwrap(), saved);
}

// Most fixtures need a reviewed healthy checkpoint; race tests pass captured tokens explicitly.
fn checkpoint(
    store: &ConfigurationStore,
    generation: &str,
) -> Result<configuration_storage::BackupSnapshot, Failure> {
    let preparation = store.prepare_backup(generation)?;
    store.create_backup(generation, &preparation.token)
}
