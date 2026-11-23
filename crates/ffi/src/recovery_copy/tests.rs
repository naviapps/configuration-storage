use super::*;
use crate::{configuration_storage_recovery_copy_request, configuration_storage_result_view_v14};
use configuration_storage::MAXIMUM_PAYLOAD_BYTES;
use serde_json::Value;
use std::{fs, path::Path, ptr};

fn request(command: &str, path: &Path, maximum: usize) -> String {
    json!({
        "command": command, "path": path, "maximumPayloadBytes": maximum
    })
    .to_string()
}

fn call(metadata: &str, payload: Option<&[u8]>) -> (u32, Value, Option<Vec<u8>>) {
    let (pointer, length) = payload.map_or((ptr::null(), 0), |bytes| (bytes.as_ptr(), bytes.len()));
    let owner = unsafe {
        Box::from_raw(configuration_storage_recovery_copy_request(
            metadata.as_ptr(),
            metadata.len(),
            pointer,
            length,
        ))
    };
    let view = unsafe { configuration_storage_result_view_v14(&*owner) };
    let json = unsafe { std::slice::from_raw_parts(view.data, view.length) };
    let output = (!view.payload.is_null())
        .then(|| unsafe { std::slice::from_raw_parts(view.payload, view.payload_length).to_vec() });
    assert_eq!(view.payload_length, output.as_ref().map_or(0, Vec::len));
    (view.code, serde_json::from_slice(json).unwrap(), output)
}

#[test]
fn invalid_requests_do_not_replace_existing_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    fs::write(&path, b"retain").unwrap();
    let path_json = serde_json::to_string(&path).unwrap();
    let invalid = (1, json!({"code": "invalidConfiguration"}), None);
    for metadata in [
        request("write", &path, 0),
        request("write", &path, MAXIMUM_PAYLOAD_BYTES + 1),
        request("write", Path::new("relative.copy"), 4),
        request("write", &directory.path().join("../copy"), 4),
        request("write", &directory.path().join("copy\0suffix"), 4),
        format!(r#"["write",{path_json},1]"#),
        format!(
            r#"{{"command":"write","path":{path_json},"maximumPayloadBytes":1,"payload":null}}"#
        ),
        format!(r#"{{"command":"read","path":{path_json},"maximumPayloadBytes":1,"payload":""}}"#),
        format!(
            r#"{{"command":"write","command":"read","path":{path_json},"maximumPayloadBytes":1}}"#
        ),
    ] {
        assert_eq!(call(&metadata, None), invalid, "{metadata}");
    }
    assert_eq!(call(&request("write", &path, 1), Some(&[0, 1])), invalid);
    assert_eq!(call(&request("read", &path, 1), Some(&[])), invalid);
    assert_eq!(fs::read(&path).unwrap(), b"retain");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn recovery_copy_has_owned_raw_bytes_and_keeps_the_exact_length_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy");
    for payload in [&[][..], &[0, 127, 128, 255]] {
        let limit = payload.len().max(1);
        assert_eq!(
            call(&request("write", &path, limit), Some(payload)),
            (0, Value::Null, None)
        );
        let (code, metadata, output) = call(&request("read", &path, limit), None);
        assert_eq!(code, 0);
        assert!(metadata.get("payload").is_none());
        assert_eq!(metadata["digest"].as_str().unwrap().len(), 64);
        assert!(metadata["modifiedAt"].as_f64().unwrap().is_finite());
        assert_eq!(output.as_deref(), Some(payload));
    }
    let file = fs::read(&path).unwrap();
    assert_eq!(
        call(&request("read", &path, 3), None),
        (1, json!({"code": "invalidRecoveryCopy"}), None)
    );
    assert_eq!(fs::read(&path).unwrap(), file);
}
