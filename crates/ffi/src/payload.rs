//! Validate base64 framing and the configured byte limit before decoding an owned payload.
use crate::{Failure, invalid};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::value::RawValue;
use std::borrow::Cow;

pub(super) fn decode(text: &str) -> Result<Vec<u8>, Failure> {
    STANDARD.decode(text).map_err(|_| invalid())
}

pub(super) fn decode_with(
    raw: &RawValue,
    maximum: usize,
    decode: impl FnOnce(&str) -> Result<Vec<u8>, Failure>,
) -> Result<Vec<u8>, Failure> {
    let encoded = payload_text(raw, maximum)?;
    // STANDARD requires canonical padded framing. Only derive its output size;
    // the base64 engine owns alphabet, padding placement and trailing-bit checks.
    let padding = encoded
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'=')
        .take(3)
        .count();
    if !encoded.len().is_multiple_of(4) || padding > 2 {
        return Err(invalid());
    }
    let decoded_length = (encoded.len() / 4 * 3)
        .checked_sub(padding)
        .ok_or_else(invalid)?;
    if decoded_length > maximum {
        return Err(invalid());
    }
    decode(&encoded)
}

fn payload_text(raw: &RawValue, maximum: usize) -> Result<Cow<'_, str>, Failure> {
    let text = raw.get();
    let body = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .ok_or_else(invalid)?;
    let maximum_encoded = base64::encoded_len(maximum, true).ok_or_else(invalid)?;
    // A JSON escape consumes at most six source bytes per base64 ASCII byte.
    if body.len() > maximum_encoded.saturating_mul(6) {
        return Err(invalid());
    }
    if body.contains('\\') {
        serde_json::from_str(text)
            .map(Cow::Owned)
            .map_err(|_| invalid())
    } else {
        if body.len() > maximum_encoded {
            return Err(invalid());
        }
        // RawValue already validated this JSON string. With no escapes its
        // body is the original base64 text.
        Ok(Cow::Borrowed(body))
    }
}

#[cfg(test)]
#[test]
fn plain_payload_text_borrows_the_original_request() {
    let input = String::from(r#""AQID""#);
    let raw: &RawValue = serde_json::from_str(&input).unwrap();
    let text = payload_text(raw, 3).unwrap();
    assert!(matches!(text, Cow::Borrowed(_)));
    assert_eq!(text.as_ptr(), input[1..].as_ptr());
}
