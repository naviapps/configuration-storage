use configuration_storage::{
    Configuration, ConfigurationStore, ErrorCode, Failure, MAXIMUM_PAYLOAD_BYTES,
};
use std::fs;

#[test]
fn invalid_configuration_is_rejected_before_either_open_mode_touches_disk() {
    let fixture = tempfile::tempdir().unwrap();
    let directory = fixture.path().join("missing-parent/store");
    let valid = Configuration {
        directory,
        owner: "com.example.fixture".into(),
        maximum_payload_bytes: 1024,
    };
    let mut invalid = vec![Configuration {
        directory: fixture.path().join("store\0suffix"),
        ..valid.clone()
    }];
    for owner in [
        "".to_owned(),
        " ".to_owned(),
        "invalid owner".to_owned(),
        "path/owner".to_owned(),
        "非ASCII".to_owned(),
        "nul\0owner".to_owned(),
        "a".repeat(1025),
    ] {
        invalid.push(Configuration {
            owner,
            ..valid.clone()
        });
    }
    for maximum_payload_bytes in [0, MAXIMUM_PAYLOAD_BYTES + 1] {
        invalid.push(Configuration {
            maximum_payload_bytes,
            ..valid.clone()
        });
    }
    for configuration in invalid {
        for open in [ConfigurationStore::open, ConfigurationStore::open_existing] {
            assert_eq!(
                open(configuration.clone()).err().expect("must reject"),
                Failure::new(ErrorCode::InvalidConfiguration)
            );
            assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 0);
        }
    }
}

#[test]
fn owner_alphabet_length_and_payload_limit_boundaries_are_shared_by_both_open_modes() {
    let fixture = tempfile::tempdir().unwrap();
    for (index, (owner, maximum_payload_bytes)) in [
        ("AZaz09._-".to_owned(), 1),
        ("a".repeat(1024), MAXIMUM_PAYLOAD_BYTES),
    ]
    .into_iter()
    .enumerate()
    {
        let configuration = Configuration {
            directory: fixture.path().join(index.to_string()),
            owner,
            maximum_payload_bytes,
        };
        let created = ConfigurationStore::open(configuration.clone()).unwrap();
        let existing = ConfigurationStore::open_existing(configuration.clone()).unwrap();
        for store in [created, existing] {
            assert_eq!(store.configuration().owner, configuration.owner);
            assert_eq!(
                store.configuration().maximum_payload_bytes,
                maximum_payload_bytes
            );
        }
    }
}
