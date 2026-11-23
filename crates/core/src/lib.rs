//! Opaque configuration snapshots with transactional writes and non-destructive recovery.
mod candidates;
mod database;
mod filesystem;
mod recovery;
pub use recovery::{
    RecoveryCopy, read_recovery_copy, validate_recovery_copy_configuration, write_recovery_copy,
};
mod save_queue;
mod store;

use base64::{Engine, display::Base64Display, engine::general_purpose::STANDARD};
pub use save_queue::{SaveCompletion, SaveQueueSnapshot, SnapshotSaveQueue};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
pub use store::ConfigurationStore;

pub const MAXIMUM_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
pub const MAXIMUM_CANDIDATES_PER_PAGE: usize = 256;
/// Maximum raw bytes for one family copy or recovery fingerprint.
pub const MAXIMUM_RAW_RECOVERY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Configuration {
    pub directory: PathBuf,
    pub owner: String,
    #[serde(default = "default_limit")]
    pub maximum_payload_bytes: usize,
}
fn default_limit() -> usize {
    16 * 1024 * 1024
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    InvalidConfiguration,
    /// Existing-only admission found no managed root.
    StoreMissing,
    OwnerMismatch,
    InvalidStoredData,
    /// The stored SQLite format version is unsupported, independent of the C ABI.
    UnsupportedFormatVersion,
    Conflict,
    Busy,
    NotInitialized,
    BackupMissing,
    BackupChanged,
    IoFailure,
    InvalidRecoveryCopy,
    UnsupportedRecoveryVersion,
    RestoreChanged,
    SnapshotCandidateMissing,
    RawRecoveryLimitExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub code: ErrorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_directory: Option<PathBuf>,
}
impl Failure {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            recovery_directory: None,
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.code)
    }
}
impl std::error::Error for Failure {}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::new(ErrorCode::IoFailure)
    }
}
impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode as SqliteCode;
        if matches!(
            error,
            rusqlite::Error::InvalidColumnType(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::Utf8Error(..)
                | rusqlite::Error::QueryReturnedNoRows
        ) {
            return Self::new(ErrorCode::InvalidStoredData);
        }
        Self::new(match error.sqlite_error_code() {
            Some(SqliteCode::DatabaseBusy | SqliteCode::DatabaseLocked) => ErrorCode::Busy,
            Some(SqliteCode::DatabaseCorrupt | SqliteCode::NotADatabase) => {
                ErrorCode::InvalidStoredData
            }
            _ => ErrorCode::IoFailure,
        })
    }
}

/// None is an uninitialized store. Some(empty bytes) is an initialized opaque payload.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub generation: Option<String>,
    #[serde(with = "optional_bytes")]
    pub payload: Option<Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSnapshot {
    pub snapshot: Snapshot,
    pub token: String,
    pub modified_at: f64,
}

/// A captured checkpoint (or its absence) and the revision required for rotation.
/// Hosts validate the returned opaque payload before using this preparation's token.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPreparation {
    pub backup: Option<BackupSnapshot>,
    pub token: String,
}

/// The managed location of a candidate, independent of its payload and editing generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotCandidateSource {
    Generation,
    Backup,
}

/// Unverified file reference. Selection is scoped to the candidate source.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCandidateReference {
    pub id: String,
    pub source: SnapshotCandidateSource,
    pub is_selected: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCandidatePage {
    pub candidates: Vec<SnapshotCandidateReference>,
    pub next_cursor: Option<String>,
}

/// Verified immutable bytes from one managed file, before host semantic validation.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCandidate {
    pub id: String,
    pub source: SnapshotCandidateSource,
    pub snapshot: Snapshot,
    pub modified_at: f64,
    pub is_selected: Option<bool>,
}

struct Base64Bytes<'a>(&'a [u8]);

impl Serialize for Base64Bytes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Base64Display::new(self.0, &STANDARD))
    }
}

mod optional_bytes {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_deref().map(Base64Bytes).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|s| STANDARD.decode(s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests;

mod bytes {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        Base64Bytes(value).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let value = String::deserialize(deserializer)?;
        STANDARD.decode(value).map_err(serde::de::Error::custom)
    }
}
