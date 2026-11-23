//! Owned synchronous C boundary; hosts schedule disk work away from their UI executor.
mod recovery_copy;
mod save_queue;
#[cfg(test)]
mod tests;
use configuration_storage::{
    BackupSnapshot, Configuration, ConfigurationStore, ErrorCode, Failure, Snapshot,
    SnapshotCandidateSource,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::ptr;

pub struct ConfigurationStorageStore(ConfigurationStore);
pub struct ConfigurationStorageResult {
    code: u32,
    bytes: Vec<u8>,
    payload: Option<Vec<u8>>,
}
#[repr(C)]
pub struct ConfigurationStorageResultView {
    pub code: u32,
    pub data: *const u8,
    pub length: usize,
    pub payload: *const u8,
    pub payload_length: usize,
}
/// Native ABI version. The SQLite schema remains version 1.
pub const CONFIGURATION_STORAGE_ABI_VERSION: u32 = 14u32;

const MAXIMUM_METADATA_BYTES: usize = 1024 * 1024;
fn invalid() -> Failure {
    Failure::new(ErrorCode::InvalidConfiguration)
}
unsafe fn input<'a>(data: *const u8, length: usize) -> Result<&'a [u8], Failure> {
    if data.is_null() || length == 0 || length > MAXIMUM_METADATA_BYTES {
        return Err(invalid());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}

struct InputPayload(*const u8, usize);

impl InputPayload {
    fn require_absent(&self) -> Result<(), Failure> {
        if self.0.is_null() && self.1 == 0 {
            Ok(())
        } else {
            Err(invalid())
        }
    }

    unsafe fn bounded(&self, maximum: usize) -> Result<&[u8], Failure> {
        if self.1 == 0 {
            return Ok(&[]);
        }
        if self.0.is_null() || self.1 > maximum {
            return Err(invalid());
        }
        Ok(unsafe { std::slice::from_raw_parts(self.0, self.1) })
    }
}

fn encode_response(value: impl Serialize) -> Result<ConfigurationStorageResult, Failure> {
    encode_with_payload(value, None)
}

fn encode_with_payload(
    value: impl Serialize,
    payload: Option<Vec<u8>>,
) -> Result<ConfigurationStorageResult, Failure> {
    let bytes = serde_json::to_vec(&value).map_err(|_| invalid())?;
    if bytes.len() > MAXIMUM_METADATA_BYTES {
        return Err(invalid());
    }
    Ok(ConfigurationStorageResult {
        code: 0,
        bytes,
        payload,
    })
}

fn snapshot_parts(snapshot: Snapshot) -> (Value, Option<Vec<u8>>) {
    (
        json!({ "generation": snapshot.generation }),
        snapshot.payload,
    )
}

fn backup_parts(backup: BackupSnapshot) -> (Value, Option<Vec<u8>>) {
    let (snapshot, payload) = snapshot_parts(backup.snapshot);
    (
        json!({ "snapshot": snapshot, "token": backup.token, "modifiedAt": backup.modified_at }),
        payload,
    )
}

fn result(
    response: Result<ConfigurationStorageResult, Failure>,
) -> *mut ConfigurationStorageResult {
    let value = match response {
        Ok(value) => value,
        Err(mut error) => {
            // JSON cannot represent every native path. Keep the failure and
            // retained files even when their optional location cannot be encoded.
            if error
                .recovery_directory
                .as_ref()
                .is_some_and(|path| path.to_str().is_none())
            {
                error.recovery_directory = None;
            }
            ConfigurationStorageResult {
                code: 1,
                bytes: serde_json::to_vec(&error).expect("stable failure"),
                payload: None,
            }
        }
    };
    Box::into_raw(Box::new(value))
}
#[unsafe(no_mangle)]
pub extern "C" fn configuration_storage_abi_version() -> u32 {
    CONFIGURATION_STORAGE_ABI_VERSION
}
/// Opens a store from bounded UTF-8 JSON, creating it if needed. Both open
/// functions perform disk I/O; schedule them away from the UI executor.
///
/// # Safety
/// Input is readable for length bytes; error is null or writable for one owned result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_store_open(
    data: *const u8,
    length: usize,
    error: *mut *mut ConfigurationStorageResult,
) -> *mut ConfigurationStorageStore {
    unsafe { open_store(data, length, error, ConfigurationStore::open) }
}
/// Opens an existing store with the same configuration and ownership contract.
/// Does not create or claim a store or create a lock for the returned handle.
/// Only an absent root yields `storeMissing`. Validate snapshots separately.
///
/// # Safety
/// Input is readable for length bytes; error is null or writable for one owned result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_store_open_existing(
    data: *const u8,
    length: usize,
    error: *mut *mut ConfigurationStorageResult,
) -> *mut ConfigurationStorageStore {
    unsafe { open_store(data, length, error, ConfigurationStore::open_existing) }
}
unsafe fn open_store(
    data: *const u8,
    length: usize,
    error: *mut *mut ConfigurationStorageResult,
    open: fn(Configuration) -> Result<ConfigurationStore, Failure>,
) -> *mut ConfigurationStorageStore {
    if !error.is_null() {
        unsafe {
            *error = std::ptr::null_mut();
        }
    }
    let value = (|| {
        let configuration: Configuration =
            serde_json::from_slice(unsafe { input(data, length)? }).map_err(|_| invalid())?;
        open(configuration)
    })();
    match value {
        Ok(value) => Box::into_raw(Box::new(ConfigurationStorageStore(value))),
        Err(failure) => {
            if !error.is_null() {
                unsafe {
                    *error = result(Err(failure));
                }
            }
            std::ptr::null_mut()
        }
    }
}
#[derive(Deserialize)]
#[serde(
    tag = "command",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Command {
    Load {},
    Save {
        expected_generation: Option<String>,
    },
    ListSnapshotCandidates {
        limit: usize,
        after: Option<String>,
        source: Option<SnapshotCandidateSource>,
    },
    ReadSnapshotCandidate {
        id: String,
    },
    PrepareBackup {
        expected_generation: String,
    },
    CreateBackup {
        expected_generation: String,
        token: String,
    },
    PrepareBackupRebuild {
        expected_generation: String,
    },
    RebuildBackup {
        expected_generation: String,
        token: String,
    },
    ReadBackup {},
    RestoreBackup {
        token: String,
    },
    PrepareRestore {},
    RestoreSnapshot {
        token: String,
    },
}
/// Dispatches UTF-8 JSON metadata (up to 1 MiB) with a separate opaque payload.
/// Omit `payload` from all JSON objects. Only `save`, `prepareRestore` and
/// `restoreSnapshot` take a payload buffer, and either pointer value with
/// length zero represents empty bytes. Other requests require null/zero.
/// Root contention returns `busy`; a committed write survives cancellation.
///
/// `prepareBackup` with `expectedGeneration` returns the optional `backup` and its `token`.
/// Validate the exact opaque backup payload, then call `createBackup` with that
/// token and generation. A token is required even for an absent backup or a
/// same-generation no-op. A changed primary or backup raw revision returns
/// `backupChanged` before replacement. Native link, type and limit checks still
/// apply. Preparation tokens do not authorize restore or rebuild. Explicit
/// rebuild is separate and retains old file families.
///
/// # Safety
/// Store stays live throughout dispatch. Nonempty buffers are readable for their
/// stated lengths until the call returns. Separate calls may run concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_store_request(
    store: *const ConfigurationStorageStore,
    data: *const u8,
    length: usize,
    payload: *const u8,
    payload_length: usize,
) -> *mut ConfigurationStorageResult {
    result((|| {
        let store = unsafe { store.as_ref() }.ok_or_else(invalid)?;
        let input = unsafe { input(data, length)? };
        let command: Command = serde_json::from_slice(input).map_err(|_| invalid())?;
        let caller_payload = InputPayload(payload, payload_length);
        let payload = if matches!(
            command,
            Command::Save { .. } | Command::PrepareRestore {} | Command::RestoreSnapshot { .. }
        ) {
            unsafe { caller_payload.bounded(store.0.configuration().maximum_payload_bytes)? }
        } else {
            caller_payload.require_absent()?;
            &[]
        };
        // Commands yielding one snapshot move its payload into the result owner.
        let (metadata, output_payload) = match command {
            Command::Load {} => snapshot_parts(store.0.load()?),
            Command::Save {
                expected_generation,
            } => snapshot_parts(store.0.save(expected_generation.as_deref(), payload)?),
            Command::ListSnapshotCandidates {
                limit,
                after,
                source,
            } => {
                return encode_response(store.0.list_snapshot_candidates(
                    limit,
                    after.as_deref(),
                    source,
                )?);
            }
            Command::ReadSnapshotCandidate { id } => {
                let candidate = store.0.read_snapshot_candidate(&id)?;
                let (snapshot, payload) = snapshot_parts(candidate.snapshot);
                (
                    json!({
                        "id": candidate.id, "source": candidate.source, "snapshot": snapshot,
                        "modifiedAt": candidate.modified_at, "isSelected": candidate.is_selected
                    }),
                    payload,
                )
            }
            Command::PrepareBackup {
                expected_generation,
            } => {
                let preparation = store.0.prepare_backup(&expected_generation)?;
                let (backup, payload) = preparation
                    .backup
                    .map(backup_parts)
                    .unwrap_or((Value::Null, None));
                (
                    json!({ "backup": backup, "token": preparation.token }),
                    payload,
                )
            }
            Command::CreateBackup {
                expected_generation,
                token,
            } => backup_parts(store.0.create_backup(&expected_generation, &token)?),
            Command::PrepareBackupRebuild {
                expected_generation,
            } => return encode_response(store.0.prepare_backup_rebuild(&expected_generation)?),
            Command::RebuildBackup {
                expected_generation,
                token,
            } => backup_parts(store.0.rebuild_backup(&expected_generation, &token)?),
            Command::ReadBackup {} => backup_parts(store.0.read_backup()?),
            Command::RestoreBackup { token } => snapshot_parts(store.0.restore_backup(&token)?),
            Command::PrepareRestore {} => {
                return encode_response(store.0.prepare_restore(payload)?);
            }
            Command::RestoreSnapshot { token } => {
                snapshot_parts(store.0.restore_snapshot(payload, &token)?)
            }
        };
        encode_with_payload(metadata, output_payload)
    })())
}
/// # Safety
/// Store is null or exclusively owned with no dispatch calls remaining.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_store_destroy(
    store: *mut ConfigurationStorageStore,
) {
    if !store.is_null() {
        drop(unsafe { Box::from_raw(store) });
    }
}
/// The payload pointer is null only when the result carries no payload.
/// A non-null pointer with length zero represents an initialized empty payload.
///
/// # Safety
/// Result is null or live. Copy borrowed bytes before destroying the result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_result_view_v14(
    value: *const ConfigurationStorageResult,
) -> ConfigurationStorageResultView {
    match unsafe { value.as_ref() } {
        Some(v) => ConfigurationStorageResultView {
            code: v.code,
            data: v.bytes.as_ptr(),
            length: v.bytes.len(),
            payload: v.payload.as_ref().map_or(ptr::null(), Vec::as_ptr),
            payload_length: v.payload.as_ref().map_or(0, Vec::len),
        },
        None => ConfigurationStorageResultView {
            code: 1,
            data: ptr::null(),
            length: 0,
            payload: ptr::null(),
            payload_length: 0,
        },
    }
}
/// # Safety
/// Result is null or exclusively owned; all borrowed views expire.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_result_destroy(
    value: *mut ConfigurationStorageResult,
) {
    if !value.is_null() {
        drop(unsafe { Box::from_raw(value) });
    }
}

/// Copies regular files independently of a store, returning an owned result.
/// Hosts validate payload semantics and choose destinations. Read requests use
/// null/zero payload; write requests use the provided buffer, possibly empty.
///
/// # Safety
/// Nonempty buffers are readable for their stated lengths until the call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_recovery_copy_request(
    data: *const u8,
    length: usize,
    payload: *const u8,
    payload_length: usize,
) -> *mut ConfigurationStorageResult {
    result(unsafe {
        input(data, length)
            .and_then(|data| recovery_copy::dispatch(data, InputPayload(payload, payload_length)))
    })
}
