//! Owned synchronous C boundary; hosts schedule disk work away from their UI executor.
mod payload;
mod recovery_copy;
mod save_queue;
mod store_payload;
#[cfg(test)]
mod tests;
use configuration_storage::{
    Configuration, ConfigurationStore, ErrorCode, Failure, SnapshotCandidateSource,
};
use serde::{Deserialize, Serialize};

pub struct ConfigurationStorageStore(ConfigurationStore);
pub struct ConfigurationStorageResult {
    code: u32,
    bytes: Vec<u8>,
}
#[repr(C)]
pub struct ConfigurationStorageResultView {
    pub code: u32,
    pub data: *const u8,
    pub length: usize,
}
const MAX_BYTES: usize = 96 * 1024 * 1024;
fn invalid() -> Failure {
    Failure::new(ErrorCode::InvalidConfiguration)
}
unsafe fn input<'a>(data: *const u8, length: usize) -> Result<&'a [u8], Failure> {
    if data.is_null() || length == 0 || length > MAX_BYTES {
        return Err(invalid());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}
fn encode_response(value: impl Serialize) -> Result<Vec<u8>, Failure> {
    serde_json::to_vec(&value).map_err(|_| invalid())
}
fn result(response: Result<Vec<u8>, Failure>) -> *mut ConfigurationStorageResult {
    let (code, bytes) = match response {
        Ok(bytes) => (0, bytes),
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
            (1, serde_json::to_vec(&error).expect("stable failure"))
        }
    };
    Box::into_raw(Box::new(ConfigurationStorageResult { code, bytes }))
}
#[unsafe(no_mangle)]
pub extern "C" fn configuration_storage_abi_version() -> u32 {
    13
}
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
    Load,
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
    ReadBackup,
    RestoreBackup {
        token: String,
    },
}
/// # Safety
/// Store stays live throughout dispatch; input is readable. Separate dispatch calls may run concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_store_dispatch(
    store: *const ConfigurationStorageStore,
    data: *const u8,
    length: usize,
) -> *mut ConfigurationStorageResult {
    result((|| {
        let store = unsafe { store.as_ref() }.ok_or_else(invalid)?;
        let input = unsafe { input(data, length)? };
        if let Some(value) = store_payload::dispatch(&store.0, input, payload::decode)? {
            return Ok(value);
        }
        let command: Command = serde_json::from_slice(input).map_err(|_| invalid())?;
        match command {
            Command::Load => encode_response(store.0.load()?),
            Command::ListSnapshotCandidates {
                limit,
                after,
                source,
            } => encode_response(store.0.list_snapshot_candidates(
                limit,
                after.as_deref(),
                source,
            )?),
            Command::ReadSnapshotCandidate { id } => {
                encode_response(store.0.read_snapshot_candidate(&id)?)
            }
            Command::PrepareBackup {
                expected_generation,
            } => encode_response(store.0.prepare_backup(&expected_generation)?),
            Command::CreateBackup {
                expected_generation,
                token,
            } => encode_response(store.0.create_backup(&expected_generation, &token)?),
            Command::PrepareBackupRebuild {
                expected_generation,
            } => encode_response(store.0.prepare_backup_rebuild(&expected_generation)?),
            Command::RebuildBackup {
                expected_generation,
                token,
            } => encode_response(store.0.rebuild_backup(&expected_generation, &token)?),
            Command::ReadBackup => encode_response(store.0.read_backup()?),
            Command::RestoreBackup { token } => encode_response(store.0.restore_backup(&token)?),
        }
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
/// # Safety
/// Result is null or live; borrowed bytes expire at destruction.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_result_view(
    value: *const ConfigurationStorageResult,
) -> ConfigurationStorageResultView {
    match unsafe { value.as_ref() } {
        Some(v) => ConfigurationStorageResultView {
            code: v.code,
            data: v.bytes.as_ptr(),
            length: v.bytes.len(),
        },
        None => ConfigurationStorageResultView {
            code: 1,
            data: std::ptr::null(),
            length: 0,
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

/// # Safety
/// Input is readable for length bytes. This performs independent regular-file I/O;
/// no store is opened. Hosts validate payload semantics and choose destinations.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn configuration_storage_recovery_copy_dispatch(
    data: *const u8,
    length: usize,
) -> *mut ConfigurationStorageResult {
    result((|| {
        recovery_copy::dispatch(unsafe { input(data, length)? })
    })())
}
