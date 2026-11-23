//! Admit recovery options before borrowing opaque bytes or touching the filesystem.
use crate::{
    ConfigurationStorageResult, Failure, InputPayload, encode_response, encode_with_payload,
    invalid,
};
use configuration_storage::{
    read_recovery_copy, validate_recovery_copy_configuration, write_recovery_copy,
};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    command: Command,
    path: PathBuf,
    maximum_payload_bytes: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Command {
    Read,
    Write,
}

pub(super) unsafe fn dispatch(
    input: &[u8],
    payload: InputPayload,
) -> Result<ConfigurationStorageResult, Failure> {
    // Derived structs also accept positional arrays; commands require an object.
    if input
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        != Some(b'{')
    {
        return Err(invalid());
    }
    let Request {
        command,
        path,
        maximum_payload_bytes,
    } = serde_json::from_slice(input).map_err(|_| invalid())?;
    match command {
        Command::Read => {
            payload.require_absent()?;
            let copy = read_recovery_copy(&path, maximum_payload_bytes)?;
            encode_with_payload(
                json!({ "digest": copy.digest, "modifiedAt": copy.modified_at }),
                Some(copy.payload),
            )
        }
        Command::Write => {
            validate_recovery_copy_configuration(&path, maximum_payload_bytes)?;
            let bytes = unsafe { payload.bounded(maximum_payload_bytes)? };
            write_recovery_copy(&path, bytes, maximum_payload_bytes)?;
            encode_response(())
        }
    }
}

#[cfg(test)]
mod tests;
