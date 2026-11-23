//! Portable, integrity-checked copies of opaque host bytes, independent of a store root.
use crate::{ErrorCode, Failure, MAXIMUM_PAYLOAD_BYTES, filesystem};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
};

const MAGIC: &[u8; 8] = b"NVCSCOPY";
const VERSION: u32 = 1;
const PREFIX_BYTES: usize = 20;
const HEADER_BYTES: usize = PREFIX_BYTES + 32;

/// Verified bytes and file information. The digest checks integrity, not authenticity.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCopy {
    #[serde(with = "crate::bytes")]
    pub payload: Vec<u8>,
    pub digest: String,
    pub modified_at: f64,
}

/// Validates a recovery-copy path and payload limit without reading or changing the filesystem.
/// Adapters can reject configuration before allocating or decoding caller payload bytes.
pub fn validate_recovery_copy_configuration(path: &Path, maximum: usize) -> Result<(), Failure> {
    if !filesystem::valid_absolute_path(path) || !(1..=MAXIMUM_PAYLOAD_BYTES).contains(&maximum) {
        return Err(Failure::new(ErrorCode::InvalidConfiguration));
    }
    Ok(())
}
fn invalid() -> Failure {
    Failure::new(ErrorCode::InvalidRecoveryCopy)
}
fn digest(prefix: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"configuration-storage-recovery-copy-v1\0");
    hash.update(prefix);
    hash.update(payload);
    hash.finalize().into()
}

/// Reads a bounded regular file without opening, creating or changing any store.
/// Hosts must validate the returned opaque payload before offering restoration.
pub fn read_recovery_copy(path: &Path, maximum: usize) -> Result<RecoveryCopy, Failure> {
    validate_recovery_copy_configuration(path, maximum)?;
    let mut file = filesystem::open_regular(path)?;
    let mut header = [0; HEADER_BYTES];
    file.read_exact(&mut header).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            invalid()
        } else {
            error.into()
        }
    })?;
    if &header[..8] != MAGIC {
        return Err(invalid());
    }
    let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
    if version != VERSION {
        return Err(Failure::new(ErrorCode::UnsupportedRecoveryVersion));
    }
    let length = u64::from_le_bytes(header[12..20].try_into().unwrap());
    if length > maximum as u64 {
        return Err(invalid());
    }
    let length = usize::try_from(length).map_err(|_| invalid())?;
    let mut payload = Vec::with_capacity(length.min(64 * 1024));
    (&mut file)
        .take(length as u64 + 1)
        .read_to_end(&mut payload)?;
    if payload.len() != length {
        return Err(invalid());
    }
    let actual = digest(&header[..PREFIX_BYTES], &payload);
    if actual.as_slice() != &header[PREFIX_BYTES..] {
        return Err(invalid());
    }
    let modified_at = filesystem::modification_time(&file.metadata()?)?;
    Ok(RecoveryCopy {
        payload,
        digest: actual.iter().map(|byte| format!("{byte:02x}")).collect(),
        modified_at,
    })
}

/// Atomically writes and syncs an explicitly selected regular file. Existing bytes
/// are replaced only after the staged copy verifies. The parent must already exist;
/// on Unix its aliases are resolved before staging without resolving the final file.
/// The host must keep destinations outside its live managed configuration directory.
pub fn write_recovery_copy(path: &Path, payload: &[u8], maximum: usize) -> Result<(), Failure> {
    validate_recovery_copy_configuration(path, maximum)?;
    if payload.len() > maximum {
        return Err(Failure::new(ErrorCode::InvalidConfiguration));
    }
    let parent = path
        .parent()
        .ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?;
    #[cfg(unix)]
    let (resolved_parent, resolved_path) = {
        // Path components discard trailing separators and '/.'. Keep such
        // directory spellings from becoming writes to a different file path.
        let bytes = path.as_os_str().as_encoded_bytes();
        if bytes.ends_with(b"/") || bytes.ends_with(b"/.") {
            return Err(Failure::new(ErrorCode::InvalidConfiguration));
        }
        let name = path
            .file_name()
            .ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?;
        // Stage and replace through the same resolved parent, without
        // following or rewriting the selected final file component.
        let parent = parent.canonicalize()?;
        let path = parent.join(name);
        (parent, path)
    };
    #[cfg(unix)]
    let (parent, path) = (resolved_parent.as_path(), resolved_path.as_path());
    filesystem::existing_directory(parent)?;
    if filesystem::exists(path)? {
        filesystem::regular(path)?;
    }
    let mut prefix = Vec::with_capacity(PREFIX_BYTES);
    prefix.extend_from_slice(MAGIC);
    prefix.extend_from_slice(&VERSION.to_le_bytes());
    prefix.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    let checksum = digest(&prefix, payload);
    let mut staged = tempfile::Builder::new()
        .prefix("configuration-recovery-")
        .make_in(parent, filesystem::private_new_file)?;
    staged.write_all(&prefix)?;
    staged.write_all(&checksum)?;
    staged.write_all(payload)?;
    staged.as_file().sync_all()?;
    if read_recovery_copy(staged.path(), maximum)?.payload != payload {
        return Err(invalid());
    }
    filesystem::atomic_replace(staged.path(), path)
}
