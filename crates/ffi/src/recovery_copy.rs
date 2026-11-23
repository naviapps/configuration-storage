//! Admit recovery options before retaining or decoding caller payload bytes.
use crate::{Failure, invalid, payload};
use configuration_storage::{
    read_recovery_copy, validate_recovery_copy_configuration, write_recovery_copy,
};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::path::PathBuf;

// An internally tagged enum buffers its fields before selecting a variant. A struct
// lets serde_json borrow the original payload, including its escaped spelling.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request<'a> {
    command: String,
    path: PathBuf,
    #[serde(rename = "maximumPayloadBytes")]
    maximum: usize,
    #[serde(default, borrow, deserialize_with = "present_payload")]
    payload: Option<&'a RawValue>,
}

fn present_payload<'a, D: Deserializer<'a>>(decoder: D) -> Result<Option<&'a RawValue>, D::Error> {
    // Preserve the difference between an absent field and an explicit JSON null.
    <&RawValue>::deserialize(decoder).map(Some)
}

pub(super) fn dispatch(input: &[u8]) -> Result<serde_json::Value, Failure> {
    dispatch_with(input, payload::decode)
}

fn dispatch_with(
    input: &[u8],
    decode: impl FnOnce(&str) -> Result<Vec<u8>, Failure>,
) -> Result<serde_json::Value, Failure> {
    // Derived structs also accept positional arrays; commands require an object.
    if input
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        != Some(b'{')
    {
        return Err(invalid());
    }
    let request: Request<'_> = serde_json::from_slice(input).map_err(|_| invalid())?;
    let value = match request.command.as_str() {
        "read" => {
            if request.payload.is_some() {
                return Err(invalid());
            }
            serde_json::to_value(read_recovery_copy(&request.path, request.maximum)?)
        }
        "write" => {
            validate_recovery_copy_configuration(&request.path, request.maximum)?;
            let payload = payload::decode_with(
                request.payload.ok_or_else(invalid)?,
                request.maximum,
                decode,
            )?;
            write_recovery_copy(&request.path, &payload, request.maximum)?;
            serde_json::to_value(())
        }
        _ => return Err(invalid()),
    };
    value.map_err(|_| invalid())
}

#[cfg(test)]
mod tests;
