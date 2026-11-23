use super::*;
use configuration_storage::{ErrorCode, MAXIMUM_PAYLOAD_BYTES};
use std::{fs, path::Path};

fn request(path: &Path, maximum: &str, payload: &str) -> String {
    format!(
        r#"{{"command":"write","path":{},"maximumPayloadBytes":{maximum},"payload":{payload}}}"#,
        serde_json::to_string(path).unwrap()
    )
}

fn no_decode(input: &str) {
    assert_eq!(
        dispatch_with(input.as_bytes(), |_| panic!(
            "Rejected request reached payload decoding"
        ))
        .unwrap_err()
        .code,
        ErrorCode::InvalidConfiguration
    );
}

#[test]
fn invalid_options_are_rejected_before_payload_decoding_or_filesystem_access() {
    let directory = tempfile::tempdir().unwrap();
    let existing = directory.path().join("copy");
    fs::write(&existing, b"retain existing bytes").unwrap();
    let missing = directory.path().join("missing-parent/copy");
    for path in [&existing, &missing] {
        for maximum in [
            "0".to_owned(),
            "-1".to_owned(),
            (MAXIMUM_PAYLOAD_BYTES + 1).to_string(),
        ] {
            no_decode(&request(path, &maximum, r#""AQID""#));
        }
    }
    for path in [
        Path::new("relative.copy"),
        directory.path().join("../copy").as_path(),
        directory.path().join("copy\0suffix").as_path(),
    ] {
        no_decode(&request(path, "3", r#""AQID""#));
    }
    assert_eq!(fs::read(existing).unwrap(), b"retain existing bytes");
    assert!(!missing.parent().unwrap().exists());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn malformed_requests_and_payload_types_never_reach_the_decoder() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    fs::write(&path, b"retain").unwrap();
    let path_json = serde_json::to_string(&path).unwrap();
    for input in [
        "{".to_owned(),
        "null".to_owned(),
        format!(r#"["write",{path_json},1,""]"#),
        format!(
            r#"{{"command":{{"write":null}},"path":{path_json},"maximumPayloadBytes":1,"payload":""}}"#
        ),
        format!(r#"{{"command":"write","path":{path_json},"maximumPayloadBytes":1}}"#),
        format!(
            r#"{{"command":"read","path":{path_json},"maximumPayloadBytes":1,"payload":null}}"#
        ),
        format!(r#"{{"command":"read","path":{path_json},"maximumPayloadBytes":1,"payload":""}}"#),
        format!(
            r#"{{"command":"write","command":"read","path":{path_json},"maximumPayloadBytes":1,"payload":""}}"#
        ),
        format!(
            r#"{{"command":"write","path":{path_json},"maximumPayloadBytes":1,"payload":"","payload":""}}"#
        ),
        format!(
            r#"{{"command":"write","path":{path_json},"maximumPayloadBytes":1,"payload":null,"payload":""}}"#
        ),
        format!(
            r#"{{"command":"write","path":{path_json},"maximumPayloadBytes":1,"payload":"","unknown":true}}"#
        ),
    ] {
        no_decode(&input);
    }
    for payload in ["null", "123", "[]", "{}", r#""\ud800""#] {
        no_decode(&request(&path, "1", payload));
    }
    assert_eq!(fs::read(&path).unwrap(), b"retain");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn oversized_plain_escaped_and_padded_payloads_are_rejected_before_decoding() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    fs::write(&path, b"retain").unwrap();
    for (payload, maximum) in [
        (format!("\"{}\"", "A".repeat(32_768)), "1"),
        (format!("\"{}\"", r"\u0041".repeat(32_768)), "1"),
        (r#""AAA=""#.to_owned(), "1"),
        (r#""AAAA""#.to_owned(), "2"),
        (r#""\u0041\u0041\u0041\u003d""#.to_owned(), "1"),
    ] {
        no_decode(&request(&path, maximum, &payload));
    }
    assert_eq!(fs::read(path).unwrap(), b"retain");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn empty_padded_boundary_and_json_escaped_payloads_retain_their_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    for (payload, maximum, expected) in [
        (r#""""#, "1", Vec::new()),
        (r#""AA==""#, "1", vec![0]),
        (r#""AAA=""#, "2", vec![0, 0]),
        (r#""AAAA""#, "3", vec![0, 0, 0]),
        (r#""\/w==""#, "1", vec![255]),
        (r#""\u002f\u0077\u003d\u003d""#, "1", vec![255]),
    ] {
        let input = request(&path, maximum, payload);
        assert_eq!(dispatch(input.as_bytes()).unwrap(), b"null");
        assert_eq!(read_recovery_copy(&path, 3).unwrap().payload, expected);
    }
    // JSON field names, operation names and order retain their ordinary serde semantics.
    let input = format!(
        r#"{{"payload":"AQID","maximumPayloadBytes":3,"path":{},"comm\u0061nd":"wr\u0069te"}}"#,
        serde_json::to_string(&path).unwrap()
    );
    dispatch(input.as_bytes()).unwrap();
    assert_eq!(read_recovery_copy(&path, 3).unwrap().payload, [1, 2, 3]);
}

#[test]
fn binding_default_limit_round_trips_and_rejects_larger_copies_without_replacement() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("boundary.copy");
    let binding_limit = 16 * 1024 * 1024;
    let payload = vec![255; binding_limit + 1];
    let maximum = binding_limit.to_string();
    let boundary = STANDARD.encode(&payload[..binding_limit]);
    let send = |input: &str| {
        let owner = unsafe {
            Box::from_raw(crate::configuration_storage_recovery_copy_dispatch(
                input.as_ptr(),
                input.len(),
            ))
        };
        let response: serde_json::Value = serde_json::from_slice(&owner.bytes).unwrap();
        (owner.code, response)
    };
    assert_eq!(
        send(&request(&path, &maximum, &format!(r#""{boundary}""#))),
        (0, serde_json::Value::Null)
    );
    let read = |limit| {
        send(&format!(
            r#"{{"command":"read","path":{},"maximumPayloadBytes":{limit}}}"#,
            serde_json::to_string(&path).unwrap()
        ))
    };
    let (status, accepted) = read(binding_limit);
    assert_eq!(status, 0);
    assert!(
        accepted["payload"].as_str() == Some(boundary.as_str()),
        "the published result must contain all admitted bytes"
    );
    let retained = fs::read(&path).unwrap();

    let (status, error) = read(binding_limit - 1);
    assert_eq!(status, 1);
    assert_eq!(error["code"], "invalidRecoveryCopy");
    assert!(fs::read(&path).unwrap() == retained);
    let oversized = STANDARD.encode(&payload);
    let (status, error) = send(&request(&path, &maximum, &format!(r#""{oversized}""#)));
    assert_eq!(status, 1);
    assert_eq!(error["code"], "invalidConfiguration");
    assert!(fs::read(&path).unwrap() == retained);
    let (status, reread) = read(binding_limit);
    assert_eq!(status, 0);
    assert!(reread == accepted);
}

#[test]
fn base64_engine_rejects_malformed_alphabet_padding_and_trailing_bits_without_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    fs::write(&path, b"retain").unwrap();
    for payload in [
        r#""@@@@""#,
        r#""AA=A""#,
        r#""A===""#,
        r#""====""#,
        r#""Zh==""#,
        r#""Z""#,
        r#""Zg""#,
        r#""Zg=""#,
    ] {
        assert_eq!(
            dispatch(request(&path, "3", payload).as_bytes())
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfiguration
        );
    }
    assert_eq!(fs::read(&path).unwrap(), b"retain");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}
