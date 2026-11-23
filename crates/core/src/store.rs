use crate::{
    BackupPreparation, BackupSnapshot, Configuration, ErrorCode, Failure, MAXIMUM_PAYLOAD_BYTES,
    MAXIMUM_RAW_RECOVERY_BYTES, Snapshot, database, filesystem,
};
use rusqlite::TransactionBehavior;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
enum Admission {
    CreateOrOpen,
    ExistingOnly,
}

enum SelectorPublication {
    Create,
    Replace,
}

#[derive(PartialEq, Eq)]
struct BackupRevision {
    primary: filesystem::SelectionFingerprint,
    backup: filesystem::BackupFingerprint,
}

struct CapturedBackup {
    path: PathBuf,
    snapshot: Option<BackupSnapshot>,
}

fn valid_owner(bytes: &[u8]) -> bool {
    (1..=1024).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
}

/// Thread-safe, file-owned store. No payload schema, callbacks, scheduler or UI.
pub struct ConfigurationStore {
    pub(crate) configuration: Configuration,
    admission: Admission,
}
impl ConfigurationStore {
    /// Open an owned root, or initialize ownership in an empty root.
    pub fn open(configuration: Configuration) -> Result<Self, Failure> {
        Self::open_with_admission(configuration, Admission::CreateOrOpen)
    }

    /// Open an already owned root without creating the root, owner marker or lock.
    /// Later operations also require the existing lock. Snapshot validation is
    /// deferred to the corresponding load or recovery operation.
    pub fn open_existing(configuration: Configuration) -> Result<Self, Failure> {
        Self::open_with_admission(configuration, Admission::ExistingOnly)
    }

    fn open_with_admission(
        mut configuration: Configuration,
        admission: Admission,
    ) -> Result<Self, Failure> {
        if !valid_owner(configuration.owner.as_bytes())
            || !(1..=MAXIMUM_PAYLOAD_BYTES).contains(&configuration.maximum_payload_bytes)
        {
            return Err(Failure::new(ErrorCode::InvalidConfiguration));
        }
        let (root, root_exists) = filesystem::resolve_root(&configuration.directory)?;
        configuration.directory = root;
        let store = Self {
            configuration,
            admission,
        };
        match admission {
            Admission::CreateOrOpen => store.initialize()?,
            Admission::ExistingOnly => {
                if !root_exists {
                    return Err(Failure::new(ErrorCode::StoreMissing));
                }
                let _lease = store.lock_existing()?;
            }
        }
        Ok(store)
    }

    fn initialize(&self) -> Result<(), Failure> {
        filesystem::directory(self.root())?;
        let marker = self.root().join("store-owner");
        let owner = self.configuration.owner.as_bytes();
        let unclaimed_entries = || -> Result<bool, Failure> {
            Ok(fs::read_dir(self.root())?
                .any(|item| item.map_or(true, |entry| entry.file_name() != "store.lock")))
        };
        let has_owner = filesystem::exists(&marker)?;
        // Check ownership before creating even a lock file in an established root.
        if has_owner && filesystem::small_file(&marker)? != owner {
            return Err(Failure::new(ErrorCode::OwnerMismatch));
        }
        let _lease = if !has_owner && unclaimed_entries()? {
            if !filesystem::exists(&self.root().join("store.lock"))? {
                return Err(Failure::new(ErrorCode::OwnerMismatch));
            }
            // An initializer may be staging ownership under an existing live lock.
            // Never create a lock in a root containing unclaimed data.
            filesystem::existing_lock(self.root())?
        } else {
            filesystem::lock(self.root())?
        };
        if filesystem::exists(&marker)? {
            if filesystem::small_file(&marker)? != owner {
                return Err(Failure::new(ErrorCode::OwnerMismatch));
            }
        } else {
            if unclaimed_entries()? {
                return Err(Failure::new(ErrorCode::OwnerMismatch));
            }
            filesystem::publish_new(&marker, owner)?;
        }
        Ok(())
    }

    fn require_existing_owner(&self) -> Result<(), Failure> {
        filesystem::existing_directory(self.root())?;
        let marker = self.root().join("store-owner");
        if !filesystem::exists(&marker)? {
            return Err(Failure::new(ErrorCode::OwnerMismatch));
        }
        let owner = filesystem::small_file(&marker)?;
        if !valid_owner(&owner) {
            return Err(Failure::new(ErrorCode::InvalidStoredData));
        }
        if owner != self.configuration.owner.as_bytes() {
            return Err(Failure::new(ErrorCode::OwnerMismatch));
        }
        Ok(())
    }

    fn lock_existing(&self) -> Result<fs::File, Failure> {
        self.require_existing_owner()?;
        let lease = filesystem::existing_lock(self.root())?;
        self.require_existing_owner()?;
        Ok(lease)
    }

    pub fn configuration(&self) -> &Configuration {
        &self.configuration
    }
    pub(crate) fn root(&self) -> &Path {
        &self.configuration.directory
    }
    pub(crate) fn lock(&self) -> Result<fs::File, Failure> {
        if matches!(self.admission, Admission::ExistingOnly) {
            return self.lock_existing();
        }
        filesystem::existing_directory(self.root())?;
        if filesystem::small_file(&self.root().join("store-owner"))?
            != self.configuration.owner.as_bytes()
        {
            return Err(Failure::new(ErrorCode::OwnerMismatch));
        }
        filesystem::lock(self.root())
    }
    fn selected(&self) -> Result<Option<PathBuf>, Failure> {
        self.selected_excluding_reservation(None)
    }
    fn selected_excluding_reservation(
        &self,
        reservation: Option<&OsStr>,
    ) -> Result<Option<PathBuf>, Failure> {
        let marker = self.root().join("active-generation");
        if !filesystem::exists(&marker)? {
            if filesystem::exists(&self.root().join("active-backup"))? {
                return Err(Failure::new(ErrorCode::InvalidStoredData));
            }
            for name in ["Generations", "Backups", "Recovery"] {
                let directory = self.root().join(name);
                if filesystem::exists(&directory)? {
                    filesystem::existing_directory(&directory)?;
                    for entry in fs::read_dir(directory)? {
                        if name == "Generations" {
                            let entry =
                                entry.map_err(|_| Failure::new(ErrorCode::InvalidStoredData))?;
                            if reservation == Some(entry.file_name().as_os_str())
                                || filesystem::is_generation_stage(&entry)?
                            {
                                continue;
                            }
                        }
                        return Err(Failure::new(ErrorCode::InvalidStoredData));
                    }
                }
            }
            return Ok(None);
        }
        let bytes = filesystem::small_file(&marker)?;
        let name =
            std::str::from_utf8(&bytes).map_err(|_| Failure::new(ErrorCode::InvalidStoredData))?;
        let id = Uuid::parse_str(name).map_err(|_| Failure::new(ErrorCode::InvalidStoredData))?;
        if id.to_string() != name {
            return Err(Failure::new(ErrorCode::InvalidStoredData));
        }
        let path = self
            .root()
            .join("Generations")
            .join(format!("{name}.sqlite"));
        filesystem::existing_directory(path.parent().unwrap())?;
        if !filesystem::exists(&path)? {
            return Err(Failure::new(ErrorCode::InvalidStoredData));
        }
        Ok(Some(path))
    }
    fn preserve_interrupted(&self, path: &Path) -> Result<Option<PathBuf>, Failure> {
        if Self::has_sidecars(path)? {
            return filesystem::preserve_family(path, self.root()).map(Some);
        }
        Ok(None)
    }
    fn preserve_error(
        &self,
        mut error: Failure,
        path: &Path,
        original: Option<PathBuf>,
    ) -> Failure {
        if error.recovery_directory.is_none() {
            // SQLite may already have consumed a journal. Expose the completed
            // pre-open capture rather than replacing it with a later copy.
            error.recovery_directory = original.or_else(|| {
                if matches!(
                    error.code,
                    ErrorCode::Busy
                        | ErrorCode::Conflict
                        | ErrorCode::OwnerMismatch
                        | ErrorCode::RawRecoveryLimitExceeded
                        | ErrorCode::UnsupportedFormatVersion
                ) {
                    None
                } else {
                    filesystem::preserve_family(path, self.root()).ok()
                }
            });
        }
        error
    }
    fn with_preserved_family<T>(
        &self,
        path: &Path,
        operation: impl FnOnce() -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let original = self.preserve_interrupted(path)?;
        operation().map_err(|error| self.preserve_error(error, path, original))
    }
    pub fn load(&self) -> Result<Snapshot, Failure> {
        let _lease = self.lock()?;
        let Some(path) = self.selected()? else {
            return Ok(Snapshot {
                generation: None,
                payload: None,
            });
        };
        self.with_preserved_family(&path, || {
            let mut connection = database::open(&path, true)?;
            let transaction = connection.transaction()?;
            let value = database::read(&transaction, &self.configuration)?;
            transaction.commit()?;
            Ok(value)
        })
    }
    fn validate_payload_size(&self, payload: &[u8]) -> Result<(), Failure> {
        if payload.len() > self.configuration.maximum_payload_bytes {
            return Err(Failure::new(ErrorCode::InvalidConfiguration));
        }
        Ok(())
    }
    pub fn save(
        &self,
        expected_generation: Option<&str>,
        payload: &[u8],
    ) -> Result<Snapshot, Failure> {
        self.validate_payload_size(payload)?;
        if expected_generation.is_some_and(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(Failure::new(ErrorCode::InvalidConfiguration));
        }
        let _lease = self.lock()?;
        let Some(path) = self.selected()? else {
            if expected_generation.is_some() {
                return Err(Failure::new(ErrorCode::Conflict));
            }
            return self.publish_generation(
                payload,
                SelectorPublication::Create,
                ErrorCode::Conflict,
                |candidate| {
                    if self
                        .selected_excluding_reservation(Some(candidate))?
                        .is_some()
                    {
                        return Err(Failure::new(ErrorCode::Conflict));
                    }
                    Ok(())
                },
            );
        };
        self.with_preserved_family(&path, || {
            let mut connection = database::open(&path, true)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = database::read(&transaction, &self.configuration)?;
            if current.generation.as_deref() != expected_generation {
                return Err(Failure::new(ErrorCode::Conflict));
            }
            if current.payload.as_deref() == Some(payload) {
                return Ok(current);
            }
            let generation = Uuid::new_v4().to_string();
            let checksum = database::digest(payload);
            let changed = transaction.execute(
                "UPDATE store_state SET generation=?1,payload=?2,digest=?3 WHERE id=1",
                rusqlite::params![generation, payload, checksum],
            )?;
            let snapshot = Snapshot {
                generation: Some(database::generation(
                    &self.configuration.owner,
                    &generation,
                    &checksum,
                )),
                payload: Some(payload.to_vec()),
            };
            database::accepted_write(&transaction, &self.configuration, &snapshot, changed)?;
            transaction.commit()?;
            Ok(snapshot)
        })
    }
    fn backup_path(&self) -> Result<PathBuf, Failure> {
        self.backup_path_at(self.root())
    }
    fn backup_path_at(&self, root: &Path) -> Result<PathBuf, Failure> {
        let directory = root.join("Backups");
        if filesystem::exists(&directory)? {
            filesystem::existing_directory(&directory)?;
        }
        let marker = root.join("active-backup");
        if filesystem::exists(&marker)? {
            let bytes = filesystem::small_file(&marker).map_err(|error| {
                if error.code == ErrorCode::InvalidStoredData {
                    self.invalid_backup_selection_at(root)
                } else {
                    error
                }
            })?;
            let name =
                std::str::from_utf8(&bytes).map_err(|_| self.invalid_backup_selection_at(root))?;
            let id = Uuid::parse_str(name).map_err(|_| self.invalid_backup_selection_at(root))?;
            if id.to_string() != name {
                return Err(self.invalid_backup_selection_at(root));
            }
            let path = directory.join(format!("{name}.sqlite"));
            if !filesystem::exists(&path)? {
                return Err(self.invalid_backup_selection_at(root));
            }
            return Ok(path);
        }
        // Existing fixed-name checkpoints remain readable until the first fresh
        // publication. Selection loss must not silently adopt a retained file.
        if filesystem::exists(&directory)? && !filesystem::legacy_backup_only(&directory)? {
            return Err(Failure::new(ErrorCode::InvalidStoredData));
        }
        Ok(directory.join("snapshot.sqlite"))
    }
    fn invalid_backup_selection_at(&self, root: &Path) -> Failure {
        let mut error = Failure::new(ErrorCode::InvalidStoredData);
        // Never expose a recovery location inside a temporary review mirror.
        if root == self.root() {
            error.recovery_directory =
                filesystem::preserve_family(&root.join("active-backup"), root).ok();
        }
        error
    }
    fn has_sidecars(path: &Path) -> Result<bool, Failure> {
        for suffix in ["-journal", "-wal", "-shm"] {
            if filesystem::exists(&filesystem::family_member(path, suffix))? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn require_clean_backup_path(&self, path: &Path) -> Result<(), Failure> {
        if filesystem::exists(path.parent().unwrap())? {
            filesystem::existing_directory(path.parent().unwrap())?;
        }
        if Self::has_sidecars(path)? {
            // Never let SQLite consume this evidence or attach it to a new main file.
            let mut error = Failure::new(ErrorCode::InvalidStoredData);
            error.recovery_directory = Some(filesystem::preserve_family(path, self.root())?);
            return Err(error);
        }
        Ok(())
    }
    fn read_backup_locked(&self) -> Result<BackupSnapshot, Failure> {
        Ok(self.read_backup_and_revision_locked()?.0)
    }
    fn read_backup_and_revision_locked(&self) -> Result<(BackupSnapshot, BackupRevision), Failure> {
        let path = self.backup_path()?;
        self.require_clean_backup_path(&path)?;
        if !filesystem::exists(&path)? {
            return Err(Failure::new(ErrorCode::BackupMissing));
        }
        let revision = self.backup_revision()?;
        // Decode the same privately verified selector and family that the
        // returned token attests, then reject changes still visible after capture.
        let snapshot = self
            .capture_backup_path(&path, &revision)?
            .snapshot
            .ok_or_else(|| Failure::new(ErrorCode::BackupChanged))?;
        #[cfg(test)]
        tests::phase(tests::Phase::ReadBackupCaptured);
        if self.backup_revision()? != revision {
            return Err(Failure::new(ErrorCode::BackupChanged));
        }
        Ok((snapshot, revision))
    }

    fn backup_snapshot(
        &self,
        snapshot: Snapshot,
        modified_at: f64,
        revision: &BackupRevision,
    ) -> BackupSnapshot {
        let mut identity = Sha256::new();
        identity.update(b"configuration-storage-backup-selection-v1");
        for bytes in [
            self.configuration.owner.as_bytes(),
            snapshot
                .generation
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            snapshot.payload.as_deref().unwrap_or_default(),
        ] {
            identity.update((bytes.len() as u64).to_le_bytes());
            identity.update(bytes);
        }
        identity.update(revision.primary.digest);
        identity.update(revision.backup.digest);
        let token = identity
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        BackupSnapshot {
            snapshot,
            token,
            modified_at,
        }
    }
    /// Returns bytes for host validation and a token binding the backup plus the current
    /// raw selection (marker and SQLite family). Refresh after any selection change,
    /// including damage or removal; no payload interpretation is used for that binding.
    pub fn read_backup(&self) -> Result<BackupSnapshot, Failure> {
        let _lease = self.lock()?;
        self.read_backup_locked()
    }

    /// Captures the selected backup's exact opaque bytes (or absence) for host validation.
    /// The expected primary generation is content-bound; preparation never grants an
    /// editing baseline, replaces a checkpoint or interprets either payload.
    pub fn prepare_backup(&self, expected_generation: &str) -> Result<BackupPreparation, Failure> {
        let _lease = self.lock()?;
        self.backup_source(expected_generation)?;
        let revision = self.backup_revision()?;
        let token = self.backup_rotation_token_at(expected_generation, &revision);
        #[cfg(test)]
        tests::phase(tests::Phase::PreparationAdmitted);
        let captured = self.capture_backup_locked(&revision)?;
        if self.backup_revision()? != revision {
            return Err(Failure::new(ErrorCode::BackupChanged));
        }
        Ok(BackupPreparation {
            backup: captured.snapshot,
            token,
        })
    }

    fn capture_backup_locked(&self, revision: &BackupRevision) -> Result<CapturedBackup, Failure> {
        let path = self.backup_path()?;
        self.require_clean_backup_path(&path)?;
        #[cfg(test)]
        tests::phase(tests::Phase::CapturePathResolved);
        self.capture_backup_path(&path, revision)
    }

    fn capture_backup_path(
        &self,
        path: &Path,
        revision: &BackupRevision,
    ) -> Result<CapturedBackup, Failure> {
        let workspace = self.stage_backup_for_review(path, revision)?;
        // A matching raw digest can describe an invalid selector or ignore an
        // unselected copied file. Resolve the PRIVATE mirror with the live store's
        // rules before choosing either returned bytes, absence or destination.
        let captured = self.backup_path_at(workspace.path())?;
        self.require_clean_backup_path(&captured)?;
        let path = self
            .root()
            .join("Backups")
            .join(captured.file_name().unwrap());
        if !filesystem::exists(&captured)? {
            return Ok(CapturedBackup {
                path,
                snapshot: None,
            });
        }
        let snapshot = database::verified(&captured, &self.configuration)
            .map_err(|error| self.preserve_error(error, &path, None))?;
        let modified_at = filesystem::modification_time(&fs::metadata(&path)?)?;
        Ok(CapturedBackup {
            path,
            snapshot: Some(self.backup_snapshot(snapshot, modified_at, revision)),
        })
    }

    fn stage_backup_for_review(
        &self,
        path: &Path,
        revision: &BackupRevision,
    ) -> Result<tempfile::TempDir, Failure> {
        let workspace = tempfile::Builder::new()
            .prefix("configuration-backup-review-")
            .tempdir()?;
        let directory = workspace.path().join("Backups");
        fs::create_dir(&directory)?;
        let marker = self.root().join("active-backup");
        if filesystem::exists(&marker)? {
            filesystem::private_new_file(&workspace.path().join("active-backup"))?
                .write_all(&filesystem::small_file(&marker)?)?;
        }
        filesystem::copy_family_into(path, &directory, MAXIMUM_RAW_RECOVERY_BYTES, false)?;
        // Compare the copied selector AND family to the exact outer revision,
        // including absence. Independent before/after source hashes could accept
        // A -> B (copied and decoded) -> A with a token that only authorizes A.
        if filesystem::backup_fingerprint(workspace.path(), None)? != revision.backup {
            return Err(Failure::new(ErrorCode::BackupChanged));
        }
        Ok(workspace)
    }

    /// Rotates only the checkpoint revision returned by prepare_backup, after host
    /// validation. The token is mandatory even when no backup exists or it already
    /// matches the primary. Reprepare and revalidate after BackupChanged.
    pub fn create_backup(
        &self,
        expected_generation: &str,
        token: &str,
    ) -> Result<BackupSnapshot, Failure> {
        let _lease = self.lock()?;
        let revision = self.backup_revision()?;
        if self.backup_rotation_token_at(expected_generation, &revision) != token {
            return Err(Failure::new(ErrorCode::BackupChanged));
        }
        let validate = |reservation: Option<&OsStr>| {
            if self.backup_revision_with_reservation(reservation)? != revision {
                return Err(Failure::new(ErrorCode::BackupChanged));
            }
            Ok(())
        };
        #[cfg(test)]
        tests::phase(tests::Phase::RotationAdmitted);
        let (source, current) = self.backup_source(expected_generation)?;
        // The approved snapshot and replacement path must come from the same
        // captured raw revision. Re-resolving a live selector after review could
        // retain path B while an A -> B -> A selector change passes validation.
        let captured = self.capture_backup_locked(&revision)?;
        #[cfg(test)]
        tests::phase(tests::Phase::RotationCaptured);
        let replace = match captured.snapshot {
            Some(previous) if previous.snapshot == current => {
                #[cfg(test)]
                tests::phase(tests::Phase::BeforeRotationPublication);
                validate(None)?;
                return Ok(previous);
            }
            Some(_) => Some(captured.path),
            None => None,
        };
        self.publish_backup(&source, &current, replace.as_deref(), validate)
    }

    fn backup_source(&self, expected_generation: &str) -> Result<(PathBuf, Snapshot), Failure> {
        let source = self
            .selected()?
            .ok_or_else(|| Failure::new(ErrorCode::NotInitialized))?;
        let current = self
            .with_preserved_family(&source, || database::verified(&source, &self.configuration))?;
        if current.generation.as_deref() != Some(expected_generation) {
            return Err(Failure::new(ErrorCode::Conflict));
        }
        Ok((source, current))
    }

    fn publish_backup(
        &self,
        source: &Path,
        current: &Snapshot,
        replace: Option<&Path>,
        validate: impl FnOnce(Option<&OsStr>) -> Result<(), Failure>,
    ) -> Result<BackupSnapshot, Failure> {
        let directory = self.root().join("Backups");
        filesystem::directory(&directory)?;
        let temporary = tempfile::Builder::new()
            .prefix(".backup-")
            .make_in(self.root(), filesystem::private_new_file)?;
        let (file, temporary) = temporary.into_parts();
        file.sync_all()?;
        drop(file);
        database::copy(source, &temporary)?;
        if database::verified(&temporary, &self.configuration)? != *current {
            return Err(Failure::new(ErrorCode::Conflict));
        }
        if let Some(destination) = replace {
            // Ordinary checkpoint rotation replaces only a verified healthy
            // selection. Reconstruction always takes the fresh-file path below.
            #[cfg(test)]
            tests::phase(tests::Phase::BeforeRotationPublication);
            validate(None)?;
            filesystem::atomic_replace(&temporary, destination)?;
            return self.read_backup_locked();
        }
        let staged_digest = filesystem::file_digest(&temporary)?
            .ok_or_else(|| Failure::new(ErrorCode::BackupChanged))?;
        let id = Uuid::new_v4().to_string();
        let filename = format!("{id}.sqlite");
        let destination = directory.join(&filename);
        filesystem::persist_new(temporary, &destination)?;
        let marker = self.root().join("active-backup");
        let result = (|| {
            filesystem::sync_directory(&directory)?;
            let selector = filesystem::staged_file(&marker, id.as_bytes())?.into_temp_path();
            #[cfg(test)]
            tests::phase(tests::Phase::FreshBackupStaged);
            #[cfg(test)]
            tests::phase(tests::Phase::BeforeRotationPublication);
            validate(Some(OsStr::new(&filename)))?;
            // The revision excludes the reserved file, so verify it after the
            // approval recheck and immediately before selecting it.
            if !Self::fresh_database_matches(&destination, staged_digest)? {
                return Err(Failure::new(ErrorCode::BackupChanged));
            }
            filesystem::atomic_replace(&selector, &marker)?;
            let published = self.read_backup_locked()?;
            if published.snapshot != *current {
                return Err(Failure::new(ErrorCode::Conflict));
            }
            Ok(published)
        })();
        if result.is_err() {
            Self::discard_unselected_database(&marker, &id, &destination, staged_digest);
        }
        result
    }

    fn fresh_database_matches(path: &Path, digest: [u8; 32]) -> Result<bool, Failure> {
        Ok(filesystem::file_digest(path)? == Some(digest) && !Self::has_sidecars(path)?)
    }

    fn discard_unselected_database(marker: &Path, id: &str, path: &Path, digest: [u8; 32]) {
        // A sync error can follow publication. Retain ambiguous selections or
        // any file that no longer matches the exclusively published main file.
        let unselected = match filesystem::exists(marker) {
            Ok(false) => true,
            Ok(true) => filesystem::small_file(marker).is_ok_and(|bytes| bytes != id.as_bytes()),
            Err(_) => false,
        };
        if unselected && Self::fresh_database_matches(path, digest) == Ok(true) {
            let _ = fs::remove_file(path);
            let _ = filesystem::sync_directory(path.parent().unwrap());
        }
    }

    fn backup_revision(&self) -> Result<BackupRevision, Failure> {
        self.backup_revision_with_reservation(None)
    }

    fn backup_revision_with_reservation(
        &self,
        reservation: Option<&OsStr>,
    ) -> Result<BackupRevision, Failure> {
        Ok(BackupRevision {
            primary: filesystem::selection_fingerprint(self.root())?,
            backup: filesystem::backup_fingerprint(self.root(), reservation)?,
        })
    }

    fn backup_rebuild_token_at(
        &self,
        expected_generation: &str,
        revision: &BackupRevision,
    ) -> String {
        let mut identity = Sha256::new();
        identity.update(b"configuration-storage-backup-rebuild-v1\0");
        for bytes in [
            self.configuration.owner.as_bytes(),
            expected_generation.as_bytes(),
        ] {
            identity.update((bytes.len() as u64).to_le_bytes());
            identity.update(bytes);
        }
        identity.update(revision.primary.digest);
        identity.update(revision.backup.digest);
        identity
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn backup_rotation_token_at(
        &self,
        expected_generation: &str,
        revision: &BackupRevision,
    ) -> String {
        // The existing raw revision binds both selections. A separate purpose
        // prevents a rebuild/restore token from authorizing destructive rotation.
        let mut identity = Sha256::new();
        identity.update(b"configuration-storage-backup-rotation-v1\0");
        identity.update(
            self.backup_rebuild_token_at(expected_generation, revision)
                .as_bytes(),
        );
        identity
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn review_backup_for_rebuild(&self, revision: &BackupRevision) -> Result<(), Failure> {
        let filesystem::BackupSelection::Checkpoint(name) = &revision.backup.selection else {
            return Ok(());
        };
        let path = self.root().join("Backups").join(name);
        let workspace = self.stage_backup_for_review(&path, revision)?;
        let captured = workspace.path().join("Backups").join(name);
        // Foreign owners and newer formats can still be readable with a journal or WAL.
        // SQLite may recover only this verified private copy, leaving the old family intact.
        match database::verified_inspection_copy(&captured, &self.configuration) {
            Ok(_) => Ok(()),
            Err(error) if error.code == ErrorCode::InvalidStoredData => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Host-selected reconstruction, bound to the exact primary and raw checkpoint.
    /// This does not grant editing permission or replace any retained backup family.
    pub fn prepare_backup_rebuild(&self, expected_generation: &str) -> Result<String, Failure> {
        let _lease = self.lock()?;
        self.backup_source(expected_generation)?;
        let revision = self.backup_revision()?;
        #[cfg(test)]
        tests::phase(tests::Phase::ReconstructionAdmitted);
        self.review_backup_for_rebuild(&revision)?;
        if self.backup_revision()? != revision {
            return Err(Failure::new(ErrorCode::BackupChanged));
        }
        Ok(self.backup_rebuild_token_at(expected_generation, &revision))
    }

    /// Selects a fresh verified checkpoint and retains the entire previous family.
    /// Foreign owners and unsupported readable formats remain refused.
    pub fn rebuild_backup(
        &self,
        expected_generation: &str,
        token: &str,
    ) -> Result<BackupSnapshot, Failure> {
        let _lease = self.lock()?;
        let revision = self.backup_revision()?;
        if self.backup_rebuild_token_at(expected_generation, &revision) != token {
            return Err(Failure::new(ErrorCode::BackupChanged));
        }
        #[cfg(test)]
        tests::phase(tests::Phase::ReconstructionAdmitted);
        let validate = |reservation: Option<&OsStr>| {
            if self.backup_revision_with_reservation(reservation)? != revision {
                return Err(Failure::new(ErrorCode::BackupChanged));
            }
            Ok(())
        };
        let (source, current) = self.backup_source(expected_generation)?;
        self.review_backup_for_rebuild(&revision)?;
        if let filesystem::BackupSelection::DamagedMarker(digest) = &revision.backup.selection {
            let marker = self.root().join("active-backup");
            let captured = filesystem::preserve_family(&marker, self.root())?;
            #[cfg(test)]
            tests::phase(tests::Phase::ReconstructionMarkerCaptured);
            // Preserve the exact damaged selector authorized by this token.
            filesystem::validate_captured_selector(
                &captured.join("active-backup"),
                *digest,
                ErrorCode::BackupChanged,
            )?;
        }
        self.publish_backup(&source, &current, None, validate)
    }
    /// Binds host-validated caller bytes to the current raw selection, including damage.
    /// This does not advance an editing baseline or permit a later changed payload.
    pub fn prepare_restore(&self, payload: &[u8]) -> Result<String, Failure> {
        self.validate_payload_size(payload)?;
        let _lease = self.lock()?;
        let revision = filesystem::selection_fingerprint(self.root())?;
        Ok(self.restore_token(payload, &revision))
    }
    fn restore_token(&self, payload: &[u8], revision: &filesystem::SelectionFingerprint) -> String {
        let mut token = Sha256::new();
        token.update(b"configuration-storage-host-restore-v1\0");
        for bytes in [self.configuration.owner.as_bytes(), payload] {
            token.update((bytes.len() as u64).to_le_bytes());
            token.update(bytes);
        }
        token.update(revision.digest);
        token
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    /// Restores exactly the reviewed bytes into a fresh generation while retaining originals.
    /// Re-review after either caller bytes or the raw selection changes.
    pub fn restore_snapshot(&self, payload: &[u8], token: &str) -> Result<Snapshot, Failure> {
        self.validate_payload_size(payload)?;
        let _lease = self.lock()?;
        let revision = filesystem::selection_fingerprint(self.root())?;
        if self.restore_token(payload, &revision) != token {
            return Err(Failure::new(ErrorCode::RestoreChanged));
        }
        let validate = || {
            if filesystem::selection_fingerprint(self.root())? != revision {
                return Err(Failure::new(ErrorCode::RestoreChanged));
            }
            Ok(())
        };
        self.restore_payload_locked(
            payload,
            revision.selector_digest,
            ErrorCode::RestoreChanged,
            validate,
        )
    }
    fn restore_payload_locked(
        &self,
        payload: &[u8],
        selector_digest: Option<[u8; 32]>,
        changed: ErrorCode,
        validate: impl Fn() -> Result<(), Failure>,
    ) -> Result<Snapshot, Failure> {
        let selector = self.root().join("active-generation");
        #[cfg(test)]
        tests::phase(tests::Phase::RestoreAdmitted);
        if let Some(digest) = selector_digest {
            if !filesystem::exists(&selector)? {
                return Err(Failure::new(changed));
            }
            // Retained databases alone cannot recover the exact old selection,
            // especially when its marker is malformed. Preserve it before any
            // new generation is prepared; capture failure must leave selection intact.
            let captured = filesystem::preserve_family(&selector, self.root())?;
            #[cfg(test)]
            tests::phase(tests::Phase::RestoreSelectorCaptured);
            filesystem::validate_captured_selector(
                &captured.join("active-generation"),
                digest,
                changed,
            )?;
        }
        let publication = if selector_digest.is_some() {
            SelectorPublication::Replace
        } else {
            SelectorPublication::Create
        };
        self.publish_generation(payload, publication, changed, |_| validate())
    }

    fn publish_generation(
        &self,
        payload: &[u8],
        publication: SelectorPublication,
        changed: ErrorCode,
        validate: impl FnOnce(&OsStr) -> Result<(), Failure>,
    ) -> Result<Snapshot, Failure> {
        let selector = self.root().join("active-generation");
        let directory = self.root().join("Generations");
        filesystem::directory(&directory)?;
        let id = Uuid::new_v4().to_string();
        let candidate = directory.join(format!("{id}.sqlite"));
        let staging = tempfile::Builder::new()
            .prefix(filesystem::GENERATION_STAGE_PREFIX)
            .rand_bytes(filesystem::GENERATION_STAGE_RANDOM_LENGTH)
            .tempdir_in(&directory)?;
        let named = tempfile::Builder::new()
            .prefix(filesystem::GENERATION_STAGE_DATABASE_PREFIX)
            .rand_bytes(filesystem::GENERATION_STAGE_RANDOM_LENGTH)
            .make_in(staging.path(), filesystem::private_new_file)?;
        let (file, temporary) = named.into_parts();
        drop(file);
        let snapshot = database::initialize(&temporary, &self.configuration, payload)?;
        filesystem::sync_file(&temporary)?;
        let digest = filesystem::file_digest(&temporary)?.ok_or_else(|| Failure::new(changed))?;
        let staged_selector = filesystem::staged_file(&selector, id.as_bytes())?.into_temp_path();
        filesystem::persist_new(temporary, &candidate)?;
        drop(staging);
        let result = (|| {
            filesystem::sync_directory(&directory)?;
            #[cfg(test)]
            tests::phase(tests::Phase::FreshGenerationStaged);
            self.require_existing_owner()?;
            validate(candidate.file_name().unwrap())?;
            if !Self::fresh_database_matches(&candidate, digest)? {
                return Err(Failure::new(changed));
            }
            match publication {
                SelectorPublication::Create => {
                    #[cfg(test)]
                    tests::phase(tests::Phase::BeforeInitialSelectorPublication);
                    filesystem::persist_new(staged_selector, &selector).map_err(|error| {
                        if error.kind() == std::io::ErrorKind::AlreadyExists {
                            Failure::new(changed)
                        } else {
                            error.into()
                        }
                    })?;
                    filesystem::sync_directory(self.root())?;
                }
                SelectorPublication::Replace => {
                    filesystem::atomic_replace(&staged_selector, &selector)?
                }
            }
            Ok(snapshot)
        })();
        if result.is_err() {
            Self::discard_unselected_database(&selector, &id, &candidate, digest);
        }
        result
    }
    /// Restores to a new retained file only while both the validated backup and raw
    /// active selection still match the supplied token. Changes return BackupChanged.
    pub fn restore_backup(&self, validated_token: &str) -> Result<Snapshot, Failure> {
        let _lease = self.lock()?;
        let (backup, revision) = self.read_backup_and_revision_locked()?;
        if backup.token != validated_token {
            return Err(Failure::new(ErrorCode::BackupChanged));
        }
        self.restore_payload_locked(
            backup.snapshot.payload.as_deref().unwrap(),
            revision.primary.selector_digest,
            ErrorCode::BackupChanged,
            || {
                if self.read_backup_locked()?.token != validated_token {
                    return Err(Failure::new(ErrorCode::BackupChanged));
                }
                Ok(())
            },
        )
    }
}
