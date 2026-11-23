//! Admit store payloads under the handle's configured limit before decoding.
use crate::{Failure, encode_response, invalid, payload};
use configuration_storage::ConfigurationStore;
use serde::{
    Deserialize, Deserializer,
    de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer},
};
use serde_json::value::RawValue;
use std::fmt;

// Internally tagged serde enums buffer their arguments before selecting a variant.
// Route by name first, then deserialize payload commands as structs that borrow the
// original JSON. The routing pass skips argument values without retaining them.
#[derive(Deserialize)]
struct CommandName {
    command: String,
}

fn command_name(input: &[u8]) -> Result<String, Failure> {
    struct NameVisitor;
    impl<'de> Visitor<'de> for NameVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a store command object or sequence")
        }

        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
            Ok(CommandName::deserialize(MapAccessDeserializer::new(map))?.command)
        }

        fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
            let name = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(0, &self))?;
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(name)
        }
    }
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let name = deserializer
        .deserialize_any(NameVisitor)
        .map_err(|_| invalid())?;
    deserializer.end().map_err(|_| invalid())?;
    Ok(name)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Save<'a> {
    #[serde(rename = "command")]
    _command: IgnoredAny,
    expected_generation: Option<String>,
    #[serde(borrow)]
    payload: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareRestore<'a> {
    #[serde(rename = "command")]
    _command: IgnoredAny,
    #[serde(borrow)]
    payload: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreSnapshot<'a> {
    #[serde(rename = "command")]
    _command: IgnoredAny,
    #[serde(borrow)]
    payload: &'a RawValue,
    token: String,
}

pub(super) fn dispatch(
    store: &ConfigurationStore,
    input: &[u8],
    decode: impl FnOnce(&str) -> Result<Vec<u8>, Failure>,
) -> Result<Option<Vec<u8>>, Failure> {
    let maximum = store.configuration().maximum_payload_bytes;
    let value = match command_name(input)?.as_str() {
        "save" => {
            let request: Save<'_> = serde_json::from_slice(input).map_err(|_| invalid())?;
            let bytes = payload::decode_with(request.payload, maximum, decode)?;
            encode_response(store.save(request.expected_generation.as_deref(), &bytes)?)
        }
        "prepareRestore" => {
            let request: PrepareRestore<'_> =
                serde_json::from_slice(input).map_err(|_| invalid())?;
            let bytes = payload::decode_with(request.payload, maximum, decode)?;
            encode_response(store.prepare_restore(&bytes)?)
        }
        "restoreSnapshot" => {
            let request: RestoreSnapshot<'_> =
                serde_json::from_slice(input).map_err(|_| invalid())?;
            let bytes = payload::decode_with(request.payload, maximum, decode)?;
            encode_response(store.restore_snapshot(&bytes, &request.token)?)
        }
        _ => return Ok(None),
    };
    value.map(Some)
}
