use super::*;
use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    PreparationAdmitted,
    RotationAdmitted,
    CapturePathResolved,
    RotationCaptured,
    BeforeRotationPublication,
    ReconstructionAdmitted,
    ReconstructionMarkerCaptured,
    RestoreAdmitted,
    RestoreSelectorCaptured,
    FreshBackupStaged,
    FreshGenerationStaged,
    BeforeInitialSelectorPublication,
    ReadBackupCaptured,
}

type PhaseHook = Box<dyn FnMut(Phase)>;
thread_local! {
    static PHASE_HOOK: RefCell<Option<PhaseHook>> = RefCell::new(None);
}

pub(super) fn phase(phase: Phase) {
    PHASE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(phase);
        }
    });
}

struct ClearPhaseHook;
impl Drop for ClearPhaseHook {
    fn drop(&mut self) {
        PHASE_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[test]
fn reconstruction_requires_a_capture_of_the_reviewed_damaged_marker() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStore::open(Configuration {
        directory: directory.path().into(),
        owner: "reconstruction.tests".into(),
        maximum_payload_bytes: 1024,
    })
    .unwrap();
    let saved = store.save(None, b"new checkpoint contents").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    store.create_backup(generation, &preparation.token).unwrap();
    let previous = store.backup_path().unwrap();
    let retained = fs::read(&previous).unwrap();

    let marker = store.root().join("active-backup");
    let reviewed = b"reviewed damaged marker";
    fs::write(&marker, reviewed).unwrap();
    let token = store.prepare_backup_rebuild(generation).unwrap();
    let hook_marker = marker.clone();
    PHASE_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |phase| match phase {
            Phase::ReconstructionAdmitted => {
                fs::write(&hook_marker, b"different damaged marker").unwrap()
            }
            Phase::ReconstructionMarkerCaptured => fs::write(&hook_marker, reviewed).unwrap(),
            _ => {}
        }))
    });
    let clear = ClearPhaseHook;
    let result = store.rebuild_backup(generation, &token);
    drop(clear);
    assert_eq!(result.unwrap_err().code, ErrorCode::BackupChanged);
    assert_eq!(fs::read(marker).unwrap(), reviewed);
    assert_eq!(fs::read(previous).unwrap(), retained);
    assert_eq!(
        fs::read_dir(store.root().join("Backups")).unwrap().count(),
        1
    );
}

#[test]
fn reconstruction_rechecks_the_previous_checkpoint_after_staging_the_new_one() {
    for legacy in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "staged.reconstruction.tests".into(),
            maximum_payload_bytes: 1024,
        })
        .unwrap();
        let first = store.save(None, b"invalid product document").unwrap();
        let generation = first.generation.as_deref().unwrap();
        let preparation = store.prepare_backup(generation).unwrap();
        store.create_backup(generation, &preparation.token).unwrap();
        let marker = store.root().join("active-backup");
        if legacy {
            fs::rename(
                store.backup_path().unwrap(),
                store.root().join("Backups/snapshot.sqlite"),
            )
            .unwrap();
            fs::remove_file(&marker).unwrap();
        }
        let selection = fs::read(&marker).ok();
        let previous = store.backup_path().unwrap();
        let old_bytes = fs::read(&previous).unwrap();
        let sidecar = filesystem::family_member(&previous, "-journal");
        let current = store
            .save(Some(generation), b"current product document")
            .unwrap();
        let current_generation = current.generation.as_deref().unwrap();
        let token = store.prepare_backup_rebuild(current_generation).unwrap();

        let edited_sidecar = sidecar.clone();
        PHASE_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |phase| {
                if phase == Phase::FreshBackupStaged {
                    fs::write(&edited_sidecar, []).unwrap();
                }
            }));
        });
        let clear = ClearPhaseHook;
        let result = store.rebuild_backup(current_generation, &token);
        drop(clear);
        assert_eq!(
            result.unwrap_err().code,
            ErrorCode::BackupChanged,
            "legacy={legacy}"
        );
        assert_eq!(fs::read(&marker).ok(), selection);
        assert_eq!(fs::read(&previous).unwrap(), old_bytes);
        assert_eq!(fs::read(&sidecar).unwrap(), b"");
        assert_eq!(
            fs::read_dir(store.root().join("Backups")).unwrap().count(),
            2
        );

        let fresh_review = store.prepare_backup_rebuild(current_generation).unwrap();
        assert_eq!(
            store
                .rebuild_backup(current_generation, &fresh_review)
                .unwrap()
                .snapshot,
            current
        );
        assert_ne!(store.backup_path().unwrap(), previous);
        assert_eq!(fs::read(previous).unwrap(), old_bytes);
        assert_eq!(fs::read(sidecar).unwrap(), b"");
    }
}

#[test]
fn fresh_backup_failure_leaves_a_competing_write_at_the_reserved_path() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStore::open(Configuration {
        directory: directory.path().into(),
        owner: "backup.reservation.tests".into(),
        maximum_payload_bytes: 1024,
    })
    .unwrap();
    let saved = store.save(None, b"current checkpoint").unwrap();
    let generation = saved.generation.as_deref().unwrap();
    let token = store.prepare_backup(generation).unwrap().token;
    let backups = store.root().join("Backups");
    let staged = backups.clone();
    let competing = b"competing write";
    PHASE_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |phase| {
            if phase == Phase::FreshBackupStaged {
                let candidate = fs::read_dir(&staged).unwrap().next().unwrap().unwrap();
                fs::write(candidate.path(), competing).unwrap();
            }
        }));
    });
    let clear = ClearPhaseHook;
    let result = store.create_backup(generation, &token);
    drop(clear);
    assert_eq!(result.unwrap_err().code, ErrorCode::BackupChanged);
    assert!(!store.root().join("active-backup").exists());
    let retained: Vec<_> = fs::read_dir(&backups)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(retained.len(), 1);
    assert_eq!(fs::read(&retained[0]).unwrap(), competing);
    assert_eq!(store.load().unwrap(), saved);
}

#[test]
fn first_save_cannot_replace_a_late_competing_selection() {
    let directory = tempfile::tempdir().unwrap();
    let open = |name| {
        ConfigurationStore::open(Configuration {
            directory: directory.path().join(name),
            owner: "initial.selection.tests".into(),
            maximum_payload_bytes: 1024,
        })
        .unwrap()
    };
    let store = open("target");
    let rival = open("rival");
    let external = rival.save(None, b"competing initialization").unwrap();
    let selected = rival.selected().unwrap().unwrap();
    let marker = fs::read(rival.root().join("active-generation")).unwrap();
    let root = store.root().to_owned();
    let written_marker = marker.clone();
    PHASE_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |phase| {
            if phase == Phase::BeforeInitialSelectorPublication {
                fs::copy(
                    &selected,
                    root.join("Generations").join(selected.file_name().unwrap()),
                )
                .unwrap();
                fs::write(root.join("active-generation"), &written_marker).unwrap();
            }
        }));
    });
    let clear = ClearPhaseHook;
    let result = store.save(None, b"uncommitted first write");
    drop(clear);
    assert_eq!(result.unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(store.load().unwrap(), external);
    assert_eq!(
        fs::read(store.root().join("active-generation")).unwrap(),
        marker
    );
    assert_eq!(
        fs::read_dir(store.root().join("Generations"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn first_save_does_not_seed_over_evidence_that_arrives_during_staging() {
    for (retained, expected) in [
        ("Recovery", ErrorCode::InvalidStoredData),
        ("Backups", ErrorCode::InvalidStoredData),
        ("-journal", ErrorCode::InvalidStoredData),
        ("", ErrorCode::Conflict),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "initial.evidence.tests".into(),
            maximum_payload_bytes: 1024,
        })
        .unwrap();
        let root = store.root().to_owned();
        let evidence = b"competing data";
        PHASE_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |phase| {
                if phase == Phase::FreshGenerationStaged {
                    let file = if retained.is_empty() || retained == "-journal" {
                        let candidate = fs::read_dir(root.join("Generations"))
                            .unwrap()
                            .next()
                            .unwrap()
                            .unwrap();
                        filesystem::family_member(&candidate.path(), retained)
                    } else {
                        fs::create_dir(root.join(retained)).unwrap();
                        root.join(retained).join("evidence")
                    };
                    fs::write(file, evidence).unwrap();
                }
            }));
        });
        let clear = ClearPhaseHook;
        let result = store.save(None, b"uncommitted first write");
        drop(clear);
        assert_eq!(result.unwrap_err().code, expected, "{retained}");
        assert!(!store.root().join("active-generation").exists());
        assert_eq!(store.load().unwrap_err().code, ErrorCode::InvalidStoredData);
        let generations = store.root().join("Generations");
        if retained.is_empty() || retained == "-journal" {
            let files = fs::read_dir(&generations)
                .unwrap()
                .map(|entry| entry.unwrap().path());
            assert!(
                files
                    .into_iter()
                    .any(|file| fs::read(file).is_ok_and(|bytes| bytes == evidence))
            );
        } else {
            assert_eq!(
                fs::read(store.root().join(retained).join("evidence")).unwrap(),
                evidence
            );
            assert_eq!(fs::read_dir(generations).unwrap().count(), 0);
        }
    }
}

#[test]
fn restores_preserve_competing_writes_at_a_fresh_generation() {
    for from_backup in [false, true] {
        for (changed_suffix, change_primary) in [("", false), ("-journal", true)] {
            let directory = tempfile::tempdir().unwrap();
            let store = ConfigurationStore::open(Configuration {
                directory: directory.path().into(),
                owner: "restore.reservation.tests".into(),
                maximum_payload_bytes: 1024,
            })
            .unwrap();
            let first = store.save(None, b"reviewed checkpoint").unwrap();
            let generation = first.generation.as_deref().unwrap();
            let preparation = store.prepare_backup(generation).unwrap();
            store.create_backup(generation, &preparation.token).unwrap();
            let current = store.save(Some(generation), b"stored primary").unwrap();
            let original = store.selected().unwrap().unwrap();
            let original_bytes = fs::read(&original).unwrap();
            let changed_primary = b"competing older-generation write";
            let competing = b"competing fresh-generation write";
            let payload = b"reviewed portable copy";
            let directory = original.parent().unwrap().to_owned();
            let hook_original = original.clone();
            let hook_directory = directory.clone();
            let token = if from_backup {
                store.read_backup().unwrap().token
            } else {
                store.prepare_restore(payload).unwrap()
            };
            PHASE_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |phase| {
                    if phase == Phase::FreshGenerationStaged {
                        let candidate = fs::read_dir(&hook_directory)
                            .unwrap()
                            .map(|entry| entry.unwrap().path())
                            .find(|path| path != &hook_original)
                            .unwrap();
                        fs::write(
                            filesystem::family_member(&candidate, changed_suffix),
                            competing,
                        )
                        .unwrap();
                        if change_primary {
                            fs::write(&hook_original, changed_primary).unwrap();
                        }
                    }
                }));
            });
            let clear = ClearPhaseHook;
            let result = if from_backup {
                store.restore_backup(&token)
            } else {
                store.restore_snapshot(payload, &token)
            };
            drop(clear);
            assert_eq!(
                result.unwrap_err().code,
                if from_backup {
                    ErrorCode::BackupChanged
                } else {
                    ErrorCode::RestoreChanged
                }
            );
            assert_eq!(store.selected().unwrap().unwrap(), original);
            assert_eq!(
                fs::read(&original).unwrap(),
                if change_primary {
                    changed_primary.as_slice()
                } else {
                    &original_bytes
                }
            );
            let retained = fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path != &original && path.extension().is_some_and(|id| id == "sqlite"))
                .unwrap();
            let member = filesystem::family_member(&retained, changed_suffix);
            assert_eq!(fs::read(&member).unwrap(), competing);

            // Return the original primary to its reviewed bytes. A new restore
            // must retain the competing unselected generation and its sidecar.
            fs::write(&original, original_bytes).unwrap();
            assert_eq!(store.load().unwrap(), current);
            let restored = if from_backup {
                store
                    .restore_backup(&store.read_backup().unwrap().token)
                    .unwrap()
            } else {
                store
                    .restore_snapshot(payload, &store.prepare_restore(payload).unwrap())
                    .unwrap()
            };
            assert_eq!(store.load().unwrap(), restored);
            assert_eq!(fs::read(member).unwrap(), competing);
        }
    }
}

#[test]
fn restores_require_a_capture_of_the_reviewed_primary_selector() {
    for from_backup in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "restore.selector.tests".into(),
            maximum_payload_bytes: 1024,
        })
        .unwrap();
        let saved = store.save(None, b"stored primary").unwrap();
        let generation = saved.generation.as_deref().unwrap();
        let preparation = store.prepare_backup(generation).unwrap();
        store.create_backup(generation, &preparation.token).unwrap();

        let marker = store.root().join("active-generation");
        let generations = store.root().join("Generations");
        let previous = generations.join(format!("{}.sqlite", fs::read_to_string(&marker).unwrap()));
        let previous_bytes = fs::read(&previous).unwrap();
        let reviewed = b"reviewed damaged primary selector";
        fs::write(&marker, reviewed).unwrap();
        let token = if from_backup {
            store.read_backup().unwrap().token
        } else {
            store.prepare_restore(b"validated external copy").unwrap()
        };
        let hook_marker = marker.clone();
        PHASE_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |phase| match phase {
                Phase::RestoreAdmitted => {
                    fs::write(&hook_marker, b"different damaged primary selector").unwrap()
                }
                Phase::RestoreSelectorCaptured => fs::write(&hook_marker, reviewed).unwrap(),
                _ => {}
            }))
        });
        let clear = ClearPhaseHook;
        let restore = || {
            if from_backup {
                store.restore_backup(&token)
            } else {
                store.restore_snapshot(b"validated external copy", &token)
            }
        };
        let result = restore();
        drop(clear);
        let changed = if from_backup {
            ErrorCode::BackupChanged
        } else {
            ErrorCode::RestoreChanged
        };
        assert_eq!(result.unwrap_err().code, changed);
        assert_eq!(fs::read(&marker).unwrap(), reviewed);
        assert_eq!(fs::read_dir(&generations).unwrap().count(), 1);

        let restored = restore().unwrap();
        assert_eq!(store.load().unwrap(), restored);
        assert_ne!(fs::read(&marker).unwrap(), reviewed);
        assert_eq!(fs::read(previous).unwrap(), previous_bytes);
        assert!(
            fs::read_dir(store.root().join("Recovery"))
                .unwrap()
                .any(|entry| {
                    fs::read(entry.unwrap().path().join("active-generation"))
                        .is_ok_and(|bytes| bytes == reviewed)
                })
        );
    }
}

fn invalid_selection_splice_fixture(canonical_missing: bool, prepare: bool) {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStore::open(Configuration {
        directory: directory.path().into(),
        owner: "staged.rotation.tests".into(),
        maximum_payload_bytes: 1024,
    })
    .unwrap();
    let original = store.save(None, b"only retained unreviewed B").unwrap();
    let generation = original.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    store.create_backup(generation, &preparation.token).unwrap();
    let path_b = store.backup_path().unwrap();
    let raw_b = fs::read(&path_b).unwrap();
    let current = store.save(Some(generation), b"current primary C").unwrap();
    let generation = current.generation.as_deref().unwrap();
    let marker = store.root().join("active-backup");
    let marker_b = fs::read(&marker).unwrap();
    let invalid = canonical_missing.then(|| Uuid::new_v4().to_string().into_bytes());
    let set_invalid = |marker: &Path, invalid: &Option<Vec<u8>>| {
        if let Some(bytes) = invalid {
            fs::write(marker, bytes).unwrap();
        } else if marker.exists() {
            fs::remove_file(marker).unwrap();
        }
    };
    set_invalid(&marker, &invalid);
    let revision = store.backup_revision().unwrap();
    let token = store.backup_rotation_token_at(generation, &revision);
    let hook_marker = marker.clone();
    let hook_invalid = invalid.clone();
    PHASE_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |phase| match phase {
            Phase::PreparationAdmitted | Phase::RotationAdmitted => {
                fs::write(&hook_marker, &marker_b).unwrap()
            }
            Phase::CapturePathResolved => {
                if let Some(bytes) = &hook_invalid {
                    fs::write(&hook_marker, bytes).unwrap();
                } else {
                    fs::remove_file(&hook_marker).unwrap();
                }
            }
            _ => {}
        }))
    });
    let clear = ClearPhaseHook;
    let error = if prepare {
        store.prepare_backup(generation).err()
    } else {
        store.create_backup(generation, &token).err()
    };
    drop(clear);
    assert!(
        fs::read(path_b).unwrap() == raw_b,
        "invalid selected A allowed unreviewed B replacement; canonical_missing={canonical_missing}"
    );
    assert_eq!(
        error.map(|error| error.code),
        Some(ErrorCode::InvalidStoredData),
        "a copied but unselected B cannot be the returned backup; canonical_missing={canonical_missing}"
    );
    assert_eq!(store.load().unwrap(), current);
}

#[test]
fn backup_preparation_refuses_copied_b_when_mirror_selection_is_invalid() {
    for canonical_missing in [true, false] {
        invalid_selection_splice_fixture(canonical_missing, true);
    }
}

#[test]
fn reading_backup_never_returns_bytes_from_another_selection() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStore::open(Configuration {
        directory: directory.path().into(),
        owner: "read.backup.selection.tests".into(),
        maximum_payload_bytes: 1024,
    })
    .unwrap();
    let first = store.save(None, b"first verified backup").unwrap();
    let generation = first.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    store.create_backup(generation, &preparation.token).unwrap();
    let marker = store.root().join("active-backup");
    let first_marker = fs::read(&marker).unwrap();
    let first_path = store.backup_path().unwrap();
    let first_bytes = fs::read(&first_path).unwrap();

    let current = store
        .save(Some(generation), b"second verified backup")
        .unwrap();
    let generation = current.generation.as_deref().unwrap();
    let preparation = store.prepare_backup_rebuild(generation).unwrap();
    store.rebuild_backup(generation, &preparation).unwrap();
    let second_marker = fs::read(&marker).unwrap();
    let second_path = store.backup_path().unwrap();
    let second_bytes = fs::read(&second_path).unwrap();
    fs::write(&marker, first_marker).unwrap();
    assert_eq!(store.read_backup().unwrap().snapshot, first);

    PHASE_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |phase| {
            if phase == Phase::ReadBackupCaptured {
                fs::write(&marker, &second_marker).unwrap();
            }
        }));
    });
    let clear = ClearPhaseHook;
    let result = store.read_backup();
    drop(clear);
    assert_eq!(result.unwrap_err().code, ErrorCode::BackupChanged);
    let observed = store.read_backup().unwrap();
    assert_eq!(observed.snapshot, current);
    assert_eq!(
        store.restore_backup(&observed.token).unwrap().payload,
        current.payload
    );
    assert_eq!(fs::read(first_path).unwrap(), first_bytes);
    assert_eq!(fs::read(second_path).unwrap(), second_bytes);
}

#[test]
fn backup_rotation_preserves_unselected_b_when_mirror_selection_is_invalid() {
    for canonical_missing in [true, false] {
        invalid_selection_splice_fixture(canonical_missing, false);
    }
}

#[test]
fn backup_rotation_rejects_staged_destination_aba_without_overwriting_unreviewed_file() {
    for attack_phase in [Phase::RotationAdmitted, Phase::RotationCaptured] {
        for primary_equals_backup in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let store = ConfigurationStore::open(Configuration {
                directory: directory.path().into(),
                owner: "staged.rotation.tests".into(),
                maximum_payload_bytes: 1024,
            })
            .unwrap();
            let original = store.save(None, b"reviewed backup A").unwrap();
            let generation = original.generation.as_deref().unwrap();
            let preparation = store.prepare_backup(generation).unwrap();
            store.create_backup(generation, &preparation.token).unwrap();
            let path_a = store.backup_path().unwrap();
            let raw_a = fs::read(&path_a).unwrap();
            let id_b = Uuid::new_v4().to_string();
            let path_b = store.root().join("Backups").join(format!("{id_b}.sqlite"));
            fs::copy(&path_a, &path_b).unwrap();
            let payload_b = b"only retained unreviewed backup B";
            let external = rusqlite::Connection::open(&path_b).unwrap();
            external
                .execute(
                    "UPDATE store_state SET payload=?1,digest=?2 WHERE id=1",
                    rusqlite::params![payload_b, database::digest(payload_b)],
                )
                .unwrap();
            drop(external);
            let raw_b = fs::read(&path_b).unwrap();
            let current = if primary_equals_backup {
                original
            } else {
                store.save(Some(generation), b"current primary C").unwrap()
            };
            let generation = current.generation.as_deref().unwrap();
            let preparation = store.prepare_backup(generation).unwrap();
            let marker = store.root().join("active-backup");
            let marker_a = fs::read(&marker).unwrap();
            let hook_marker = marker.clone();
            let hook_marker_a = marker_a.clone();
            PHASE_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |phase| {
                    if phase == attack_phase {
                        fs::write(&hook_marker, id_b.as_bytes()).unwrap();
                    } else if phase == Phase::BeforeRotationPublication {
                        fs::write(&hook_marker, &hook_marker_a).unwrap();
                    }
                }))
            });
            let clear = ClearPhaseHook;
            let result = store.create_backup(generation, &preparation.token);
            drop(clear);
            // Early refusal can leave the external selector at B; the test restores
            // only that external edit before inspecting either stable database.
            fs::write(marker, marker_a).unwrap();
            assert!(
                fs::read(&path_b).unwrap() == raw_b,
                "unreviewed B was overwritten; primary_equals_backup={primary_equals_backup}"
            );
            if attack_phase == Phase::RotationAdmitted {
                assert_eq!(fs::read(&path_a).unwrap(), raw_a);
                assert_eq!(
                    result.err().map(|error| error.code),
                    Some(ErrorCode::BackupChanged)
                );
            } else {
                // A selector detour after capture cannot redirect the held target.
                // Completion writes or reuses A while leaving unreviewed B intact.
                assert_eq!(result.unwrap().snapshot, current);
                if primary_equals_backup {
                    assert_eq!(fs::read(&path_a).unwrap(), raw_a);
                }
            }
            assert_eq!(store.load().unwrap(), current);
            assert!(!store.root().join("Recovery").exists());
        }
    }
}

#[test]
fn backup_preparation_rejects_staged_aba_between_revision_and_capture() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStore::open(Configuration {
        directory: directory.path().into(),
        owner: "staged.rotation.tests".into(),
        maximum_payload_bytes: 1024,
    })
    .unwrap();
    let original = store.save(None, b"reviewed backup A").unwrap();
    let generation = original.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    store.create_backup(generation, &preparation.token).unwrap();
    let current = store.save(Some(generation), b"current primary").unwrap();
    let generation = current.generation.as_deref().unwrap();
    let revision = store.backup_revision().unwrap();
    let initial = store.backup_rotation_token_at(generation, &revision);
    let path = store.backup_path().unwrap();
    let raw_a = fs::read(&path).unwrap();

    // Deterministically model an uncooperative writer at the private phase
    // boundaries, rather than relying on concurrent timing or host callbacks.
    let payload_b = b"unreviewed backup B";
    let external = rusqlite::Connection::open(&path).unwrap();
    external
        .execute(
            "UPDATE store_state SET payload=?1,digest=?2 WHERE id=1",
            rusqlite::params![payload_b, database::digest(payload_b)],
        )
        .unwrap();
    drop(external);
    let captured = store.capture_backup_locked(&revision);
    fs::write(&path, &raw_a).unwrap();
    assert_eq!(
        store.backup_rotation_token_at(generation, &store.backup_revision().unwrap()),
        initial
    );
    assert_eq!(
        captured.err().map(|error| error.code),
        Some(ErrorCode::BackupChanged)
    );
    assert_eq!(fs::read(path).unwrap(), raw_a);
    assert_eq!(store.load().unwrap(), current);
}

#[test]
fn backup_preparation_rejects_staged_marker_aba_with_two_stable_files() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigurationStore::open(Configuration {
        directory: directory.path().into(),
        owner: "staged.rotation.tests".into(),
        maximum_payload_bytes: 1024,
    })
    .unwrap();
    let original = store.save(None, b"reviewed backup A").unwrap();
    let generation = original.generation.as_deref().unwrap();
    let preparation = store.prepare_backup(generation).unwrap();
    store.create_backup(generation, &preparation.token).unwrap();
    let current = store
        .save(Some(generation), b"unreviewed backup B")
        .unwrap();
    let id_b = Uuid::new_v4().to_string();
    let path_b = store.root().join("Backups").join(format!("{id_b}.sqlite"));
    fs::copy(store.selected().unwrap().unwrap(), &path_b).unwrap();
    let marker = store.root().join("active-backup");
    let marker_a = fs::read(&marker).unwrap();
    let path_a = store.backup_path().unwrap();
    let raw_a = fs::read(&path_a).unwrap();
    let raw_b = fs::read(&path_b).unwrap();
    let revision = store.backup_revision().unwrap();

    fs::write(&marker, id_b.as_bytes()).unwrap();
    let captured = store.capture_backup_locked(&revision);
    fs::write(&marker, marker_a).unwrap();
    assert!(store.backup_revision().unwrap() == revision);
    assert_eq!(
        captured.err().map(|error| error.code),
        Some(ErrorCode::BackupChanged)
    );
    assert_eq!(fs::read(path_a).unwrap(), raw_a);
    assert_eq!(fs::read(path_b).unwrap(), raw_b);
    assert_eq!(store.load().unwrap(), current);
}

#[test]
fn backup_preparation_rejects_staged_legacy_presence_aba() {
    for initially_present in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigurationStore::open(Configuration {
            directory: directory.path().into(),
            owner: "staged.rotation.tests".into(),
            maximum_payload_bytes: 1024,
        })
        .unwrap();
        let original = store.save(None, b"reviewed primary").unwrap();
        let generation = original.generation.as_deref().unwrap();
        let preparation = store.prepare_backup(generation).unwrap();
        store.create_backup(generation, &preparation.token).unwrap();
        let path = store.root().join("Backups/snapshot.sqlite");
        fs::rename(store.backup_path().unwrap(), &path).unwrap();
        fs::remove_file(store.root().join("active-backup")).unwrap();
        let raw = fs::read(&path).unwrap();
        if !initially_present {
            fs::remove_file(&path).unwrap();
        }
        let revision = store.backup_revision().unwrap();
        if initially_present {
            fs::remove_file(&path).unwrap();
        } else {
            fs::write(&path, &raw).unwrap();
        }
        let captured = store.capture_backup_locked(&revision);
        if initially_present {
            fs::write(&path, &raw).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        assert!(store.backup_revision().unwrap() == revision);
        assert_eq!(
            captured.err().map(|error| error.code),
            Some(ErrorCode::BackupChanged)
        );
        assert_eq!(store.load().unwrap(), original);
    }
}
