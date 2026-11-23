use configuration_storage::{Configuration, ConfigurationStore, ErrorCode, Snapshot};
use std::path::PathBuf;

fn main() {
    let owned = PathBuf::from(std::env::args_os().nth(1).expect("owned fixture directory"));
    assert!(owned.is_absolute());
    let configuration = Configuration {
        directory: owned.join("store"),
        owner: "com.example.standalone.storage".into(),
        maximum_payload_bytes: 4096,
    };
    let writer = ConfigurationStore::open(configuration.clone()).unwrap();
    assert_eq!(
        writer.load().unwrap(),
        Snapshot {
            generation: None,
            payload: None,
        }
    );

    let initial = [0, 0xff, 0x80, 0];
    let first = writer.save(None, &initial).unwrap();
    assert_eq!(first.payload.as_deref(), Some(initial.as_slice()));
    let generation = first.generation.as_deref().expect("initialized generation");
    let preparation = writer.prepare_backup(generation).unwrap();
    assert!(preparation.backup.is_none());
    let first_backup = writer
        .create_backup(generation, &preparation.token)
        .unwrap();
    assert_eq!(first_backup.snapshot, first);
    let old_review = writer.prepare_backup(generation).unwrap();
    assert_eq!(old_review.backup.unwrap(), first_backup);

    let reopened = ConfigurationStore::open(configuration).unwrap();
    assert_eq!(reopened.load().unwrap(), first);
    let replacement = [0xff, 0, 0x7f, 1];
    let latest = reopened.save(Some(generation), &replacement).unwrap();
    assert_eq!(latest.payload.as_deref(), Some(replacement.as_slice()));
    assert!(latest.generation.is_some());
    assert_ne!(latest.generation, first.generation);
    assert_eq!(
        writer
            .create_backup(generation, &old_review.token)
            .unwrap_err()
            .code,
        ErrorCode::BackupChanged
    );
    let latest_generation = latest.generation.as_deref().unwrap();
    let reviewed = writer.prepare_backup(latest_generation).unwrap();
    assert_eq!(reviewed.backup.unwrap().snapshot.payload, first.payload);
    assert_eq!(
        writer
            .create_backup(latest_generation, &reviewed.token)
            .unwrap()
            .snapshot,
        latest
    );

    let stale = writer.save(Some(generation), b"stale").unwrap_err();
    assert_eq!(stale.code, ErrorCode::Conflict);
    assert_eq!(writer.load().unwrap(), latest);
    println!(
        "Standalone Rust Git ConfigurationStorage consumer passed opaque bytes, generation conflict and reviewed backup rotation."
    );
}
