//! Explicit bounded inspection of managed snapshot files; never chooses a recovery policy.
use crate::{
    ConfigurationStore, ErrorCode, Failure, MAXIMUM_CANDIDATES_PER_PAGE,
    MAXIMUM_RAW_RECOVERY_BYTES, SnapshotCandidate, SnapshotCandidatePage,
    SnapshotCandidateReference, SnapshotCandidateSource, database, filesystem,
};
use std::{collections::BTreeMap, fs, path::Path};
const BACKUP_PREFIX: &str = "backup:";
const LEGACY_BACKUP: &str = "snapshot";

impl SnapshotCandidateSource {
    fn directory(self) -> &'static str {
        match self {
            Self::Generation => "Generations",
            Self::Backup => "Backups",
        }
    }

    fn selector(self) -> &'static str {
        match self {
            Self::Generation => "active-generation",
            Self::Backup => "active-backup",
        }
    }

    fn id(self, stem: &str) -> String {
        match self {
            Self::Generation => stem.to_owned(),
            Self::Backup => format!("{BACKUP_PREFIX}{stem}"),
        }
    }

    fn accepts(self, stem: &str) -> bool {
        filesystem::canonical_uuid(stem) || (self == Self::Backup && stem == LEGACY_BACKUP)
    }
}

fn parse_id(id: &str) -> Option<(SnapshotCandidateSource, &str)> {
    let (source, stem) = match id.strip_prefix(BACKUP_PREFIX) {
        Some(stem) => (SnapshotCandidateSource::Backup, stem),
        None => (SnapshotCandidateSource::Generation, id),
    };
    source.accepts(stem).then_some((source, stem))
}

fn source_failure(path: &Path, expected: Option<[u8; 32]>, error: Failure) -> Failure {
    if matches!(error.code, ErrorCode::IoFailure | ErrorCode::Busy) {
        if matches!(filesystem::exists(path), Ok(false)) {
            return Failure::new(ErrorCode::SnapshotCandidateMissing);
        }
        if let Some(expected) = expected
            && filesystem::bounded_family_fingerprint(path, MAXIMUM_RAW_RECOVERY_BYTES)
                .is_ok_and(|current| current != expected)
        {
            return Failure::new(ErrorCode::Busy);
        }
    }
    error
}

impl ConfigurationStore {
    /// Selection is advisory for read-only candidates. A failed lookup must
    /// not prevent listing or reading independently verifiable data.
    fn candidate_selection(&self, source: SnapshotCandidateSource) -> Option<String> {
        self.read_candidate_selection(source).ok().flatten()
    }

    /// Only used for candidate metadata, never to authorize a write or restore.
    fn read_candidate_selection(
        &self,
        source: SnapshotCandidateSource,
    ) -> Result<Option<String>, Failure> {
        let marker = self.root().join(source.selector());
        if !filesystem::exists(&marker)? {
            let directory = self.root().join(source.directory());
            if source == SnapshotCandidateSource::Backup
                && filesystem::exists(&directory.join("snapshot.sqlite"))?
                && filesystem::legacy_backup_only(&directory)?
            {
                return Ok(Some(source.id(LEGACY_BACKUP)));
            }
            return Ok(None);
        }
        let bytes = filesystem::small_file(&marker)?;
        Ok(String::from_utf8(bytes)
            .ok()
            .filter(|value| filesystem::canonical_uuid(value))
            .map(|stem| source.id(&stem)))
    }

    /// Lists unverified references, not per-save history or restore authorization.
    /// None scans both sources. A scoped query requires a cursor from that source.
    /// Calls are individually locked. Restart paging to rescan concurrent file changes.
    pub fn list_snapshot_candidates(
        &self,
        limit: usize,
        after: Option<&str>,
        source: Option<SnapshotCandidateSource>,
    ) -> Result<SnapshotCandidatePage, Failure> {
        if !(1..=MAXIMUM_CANDIDATES_PER_PAGE).contains(&limit)
            || after.is_some_and(|value| {
                parse_id(value).is_none_or(|(cursor_source, _)| {
                    source.is_some_and(|requested| requested != cursor_source)
                })
            })
        {
            return Err(Failure::new(ErrorCode::InvalidConfiguration));
        }
        let _lease = self.lock()?;
        let mut ids = BTreeMap::new();
        let all_sources = [
            SnapshotCandidateSource::Generation,
            SnapshotCandidateSource::Backup,
        ];
        let sources = if source.is_some() {
            source.as_slice()
        } else {
            &all_sources
        };
        for &source in sources {
            let directory = self.root().join(source.directory());
            if !filesystem::exists(&directory)? {
                continue;
            }
            filesystem::existing_directory(&directory)?;
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if !kind.is_file() || kind.is_symlink() {
                    continue;
                }
                let name = entry.file_name();
                let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".sqlite")) else {
                    continue;
                };
                if !source.accepts(stem) {
                    continue;
                }
                let id = source.id(stem);
                if after.is_none_or(|cursor| id.as_str() > cursor) {
                    ids.insert(id, source);
                    if ids.len() > limit + 1 {
                        ids.pop_last();
                    }
                }
            }
        }
        let more = ids.len() > limit;
        if more {
            ids.pop_last();
        }
        let next_cursor = more.then(|| ids.last_key_value().unwrap().0.clone());
        let mut generations = None;
        let mut backups = None;
        let mut candidates = Vec::with_capacity(ids.len());
        for (id, source) in ids {
            let selection = match source {
                SnapshotCandidateSource::Generation => &mut generations,
                SnapshotCandidateSource::Backup => &mut backups,
            };
            if selection.is_none() {
                *selection = Some(self.candidate_selection(source));
            }
            candidates.push(SnapshotCandidateReference {
                is_selected: selection
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .map(|selected| selected == &id),
                id,
                source,
            });
        }
        Ok(SnapshotCandidatePage {
            candidates,
            next_cursor,
        })
    }

    /// Reads immutable verified bytes from a private copy. Original main/journal/WAL/SHM
    /// files are never opened by SQLite, changed or removed by this inspection.
    pub fn read_snapshot_candidate(&self, id: &str) -> Result<SnapshotCandidate, Failure> {
        let (source, stem) =
            parse_id(id).ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?;
        let _lease = self.lock()?;
        let directory = self.root().join(source.directory());
        let path = directory.join(format!("{stem}.sqlite"));
        let mut identity = None;
        let result = (|| {
            if !filesystem::exists(&directory)? {
                return Err(Failure::new(ErrorCode::SnapshotCandidateMissing));
            }
            filesystem::existing_directory(&directory)?;
            if !filesystem::exists(&path)? {
                return Err(Failure::new(ErrorCode::SnapshotCandidateMissing));
            }
            filesystem::regular(&path)?;
            let expected =
                filesystem::bounded_family_fingerprint(&path, MAXIMUM_RAW_RECOVERY_BYTES)?;
            identity = Some(expected);
            let modified_at = filesystem::modification_time(&fs::metadata(&path)?)?;
            let workspace = tempfile::Builder::new()
                .prefix("configuration-inspection-")
                .tempdir()?;
            filesystem::copy_family_into(
                &path,
                workspace.path(),
                MAXIMUM_RAW_RECOVERY_BYTES,
                false,
            )?;
            let staged = workspace.path().join(path.file_name().unwrap());
            if filesystem::bounded_family_fingerprint(&staged, MAXIMUM_RAW_RECOVERY_BYTES)?
                != expected
                || filesystem::bounded_family_fingerprint(&path, MAXIMUM_RAW_RECOVERY_BYTES)?
                    != expected
            {
                return Err(Failure::new(ErrorCode::Busy));
            }
            let snapshot = database::verified_inspection_copy(&staged, &self.configuration);
            if filesystem::bounded_family_fingerprint(&path, MAXIMUM_RAW_RECOVERY_BYTES)?
                != expected
            {
                return Err(Failure::new(ErrorCode::Busy));
            }
            Ok(SnapshotCandidate {
                id: id.to_owned(),
                source,
                snapshot: snapshot?,
                modified_at,
                is_selected: self
                    .candidate_selection(source)
                    .as_ref()
                    .map(|selected| selected == id),
            })
        })();
        result.map_err(|error| source_failure(&path, identity, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disappearance_and_changed_source_are_distinct_from_genuine_io_failure() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("candidate.sqlite");
        fs::write(&path, b"before").unwrap();
        let expected =
            filesystem::bounded_family_fingerprint(&path, MAXIMUM_RAW_RECOVERY_BYTES).unwrap();
        assert_eq!(
            source_failure(&path, Some(expected), Failure::new(ErrorCode::IoFailure)).code,
            ErrorCode::IoFailure
        );
        fs::write(&path, b"after").unwrap();
        assert_eq!(
            source_failure(&path, Some(expected), Failure::new(ErrorCode::IoFailure)).code,
            ErrorCode::Busy
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            source_failure(&path, Some(expected), Failure::new(ErrorCode::IoFailure)).code,
            ErrorCode::SnapshotCandidateMissing
        );
        assert_eq!(
            source_failure(
                &path,
                Some(expected),
                Failure::new(ErrorCode::OwnerMismatch)
            )
            .code,
            ErrorCode::OwnerMismatch
        );
    }
}
