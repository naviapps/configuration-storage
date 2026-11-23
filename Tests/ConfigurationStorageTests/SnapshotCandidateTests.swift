import Foundation
import XCTest

@testable import ConfigurationStorage

@MainActor
final class SnapshotCandidateTests: XCTestCase {
  func testNativeCandidateCursorCarriesOwnedBytesIntoRestore() async throws {
    let fixture = SnapshotCandidateFixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let payload = Data([128, 0, 66, 254])
    let original = try await store.save(expectedGeneration: nil, payload: payload)
    let replacement = Data([7])
    let prepareReplacement = try await store.prepareRestore(payload: replacement)
    _ = try await store.restoreSnapshot(payload: replacement, token: prepareReplacement)

    let first = try await store.listSnapshotCandidates(limit: 1, source: .generation)
    let cursor = try XCTUnwrap(first.nextCursor)
    let second = try await store.listSnapshotCandidates(
      limit: 1, after: cursor, source: .generation)
    let reference = try XCTUnwrap(
      (first.candidates + second.candidates).first { $0.isSelected == false })
    let held = try await store.readSnapshotCandidate(id: reference.id)
    XCTAssertEqual(held.id, reference.id)
    XCTAssertEqual(held.source, .generation)
    XCTAssertEqual(held.snapshot, original)
    let bytes = try XCTUnwrap(held.snapshot.payload)
    let token = try await store.prepareRestore(payload: bytes)
    let restored = try await store.restoreSnapshot(payload: bytes, token: token)
    XCTAssertEqual(restored.payload, payload)
    XCTAssertEqual(held.snapshot, original)
  }

  func testMissingNativeCandidateMapsTheStorageError() async throws {
    let fixture = SnapshotCandidateFixture()
    defer { fixture.remove() }
    do {
      _ = try await fixture.store().readSnapshotCandidate(id: UUID().uuidString.lowercased())
      XCTFail("A missing candidate must report snapshotCandidateMissing.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .snapshotCandidateMissing)
    }
  }

  func testCancelledCandidateOperationsDoNotOpenStorage() async throws {
    for reading in [false, true] {
      let fixture = SnapshotCandidateFixture()
      defer { fixture.remove() }
      let store = fixture.store()
      let operation = Task {
        withUnsafeCurrentTask { $0?.cancel() }
        if reading {
          _ = try await store.readSnapshotCandidate(id: "not-entered")
        } else {
          _ = try await store.listSnapshotCandidates()
        }
      }
      do {
        try await operation.value
        XCTFail("Pre-cancelled candidate operations must stop before native entry.")
      } catch is CancellationError {}
      XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))
    }
  }

  func testReferencesRoundTripOpaqueIDsAndRequiredNullableSelection() throws {
    for id in ["opaque-candidate/\"\\\n日本語", " "] {
      for selection in [true, false, NSNull()] as [Any] {
        let reference = try decode(
          ConfigurationSnapshotCandidateReference.self,
          object: ["id": id, "source": "backup", "isSelected": selection])
        XCTAssertEqual(reference.id, id)
        XCTAssertEqual(reference.isSelected, selection as? Bool)
        let encoded = try JSONEncoder().encode(reference)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        XCTAssertEqual(Set(object.keys), ["id", "source", "isSelected"])
        if selection is NSNull { XCTAssertTrue(object["isSelected"] is NSNull) }
        XCTAssertEqual(
          try JSONDecoder().decode(ConfigurationSnapshotCandidateReference.self, from: encoded),
          reference)
      }
    }
    for object in [
      ["isSelected": NSNull()], ["id": "opaque"], ["id": "", "isSelected": false],
      ["id": NSNull(), "isSelected": false], ["id": 1, "isSelected": false],
      ["id": "opaque", "isSelected": 1], ["id": "opaque", "isSelected": "false"],
    ] as [[String: Any]] {
      assertInvalid(
        ConfigurationSnapshotCandidateReference.self,
        object: object.merging(["source": "backup"]) { current, _ in current })
    }
  }

  func testPagesRequireBoundedSortedUniqueReferencesAndMatchingNullableCursor() throws {
    let references: [[String: Any]] = (0..<256).map {
      ["source": "generation", "id": String(format: "opaque-%03d", $0), "isSelected": NSNull()]
    }
    for cursor in [NSNull(), "opaque-255"] as [Any] {
      let page = try decode(
        ConfigurationSnapshotCandidatePage.self,
        object: ["candidates": references, "nextCursor": cursor])
      XCTAssertEqual(page.candidates.count, 256)
      XCTAssertEqual(page.nextCursor, cursor as? String)
      let encoded = try JSONEncoder().encode(page)
      XCTAssertEqual(
        try JSONDecoder().decode(ConfigurationSnapshotCandidatePage.self, from: encoded), page)
      let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
      if cursor is NSNull { XCTAssertTrue(object["nextCursor"] is NSNull) }
    }
    let empty = try decode(
      ConfigurationSnapshotCandidatePage.self, object: ["candidates": [], "nextCursor": NSNull()])
    XCTAssertTrue(empty.candidates.isEmpty)
    for object in [
      ["candidates": references], ["nextCursor": NSNull()],
      ["candidates": NSNull(), "nextCursor": NSNull()],
      [
        "candidates": references + [
          ["source": "generation", "id": "opaque-256", "isSelected": NSNull()]
        ],
        "nextCursor": NSNull(),
      ],
      ["candidates": [references[1], references[0]], "nextCursor": NSNull()],
      ["candidates": [references[0], references[0]], "nextCursor": NSNull()],
      ["candidates": references, "nextCursor": "opaque-254"],
      ["candidates": references, "nextCursor": ""],
      ["candidates": references, "nextCursor": false],
      ["candidates": [], "nextCursor": "opaque-000"],
    ] as [[String: Any]] {
      assertInvalid(ConfigurationSnapshotCandidatePage.self, object: object)
    }
  }

  func testCandidatesRoundTripInitializedEmptyOrBinaryPayloadAndSignedUnixSeconds() throws {
    for payload in [Data(), Data([0, 255, 128, 0, 254])] {
      for seconds in [-11_644_473_600.0, -1.25, 0, 1_700_000_000.25, -1e-9, 2e-9] {
        for selection in [true, false, NSNull()] as [Any] {
          var object = candidateObject
          object["snapshot"] = [
            "generation": "opaque-write", "payload": payload.base64EncodedString(),
          ]
          object["modifiedAt"] = seconds
          object["isSelected"] = selection
          let candidate = try decode(ConfigurationSnapshotCandidate.self, object: object)
          XCTAssertEqual(candidate.id, "opaque-file/日本語")
          XCTAssertEqual(candidate.snapshot.generation, "opaque-write")
          XCTAssertEqual(candidate.snapshot.payload, payload)
          XCTAssertEqual(candidate.modifiedAtUnixSeconds, seconds)
          XCTAssertEqual(candidate.modifiedAt, Date(timeIntervalSince1970: seconds))
          XCTAssertEqual(candidate.isSelected, selection as? Bool)
          let encoded = try JSONEncoder().encode(candidate)
          XCTAssertEqual(
            try JSONDecoder().decode(ConfigurationSnapshotCandidate.self, from: encoded), candidate)
          let encodedObject = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any])
          XCTAssertEqual(encodedObject["modifiedAt"] as? Double, seconds)
          if selection is NSNull { XCTAssertTrue(encodedObject["isSelected"] is NSNull) }
        }
      }
    }
  }

  func testCandidatesRejectMissingKeysInvalidIdentityUninitializedSnapshotAndWrongTypes() {
    for key in ["id", "source", "snapshot", "modifiedAt", "isSelected"] {
      var object = candidateObject
      object.removeValue(forKey: key)
      assertInvalid(ConfigurationSnapshotCandidate.self, object: object)
    }
    let invalidFields: [(String, Any)] = [
      ("id", ""), ("id", NSNull()), ("id", 1),
      ("snapshot", NSNull()), ("snapshot", ["generation": NSNull(), "payload": NSNull()]),
      ("snapshot", ["generation": "", "payload": ""]),
      ("snapshot", ["generation": "opaque", "payload": "invalid base64!"]),
      ("modifiedAt", NSNull()), ("modifiedAt", "-1.25"), ("modifiedAt", true),
      ("source", "unknown"), ("isSelected", 0), ("isSelected", "true"),
    ]
    for (key, value) in invalidFields {
      var object = candidateObject
      object[key] = value
      assertInvalid(ConfigurationSnapshotCandidate.self, object: object)
    }
  }

  func testCandidatesRejectNonfiniteModificationTimesEvenWhenDecoderAllowsThem() throws {
    let decoder = JSONDecoder()
    decoder.nonConformingFloatDecodingStrategy = .convertFromString(
      positiveInfinity: "Infinity", negativeInfinity: "-Infinity", nan: "NaN")
    for seconds in ["Infinity", "-Infinity", "NaN"] {
      var object = candidateObject
      object["modifiedAt"] = seconds
      let json = try JSONSerialization.data(withJSONObject: object)
      XCTAssertThrowsError(try decoder.decode(ConfigurationSnapshotCandidate.self, from: json)) {
        guard case DecodingError.dataCorrupted(let context) = $0 else {
          return XCTFail("Expected nonfinite timestamp rejection, received \($0)")
        }
        XCTAssertEqual(context.codingPath.map(\.stringValue), ["modifiedAt"])
      }
    }
  }

  private var candidateObject: [String: Any] {
    [
      "id": "opaque-file/日本語", "snapshot": ["generation": "opaque-write", "payload": ""],
      "modifiedAt": 0, "source": "backup", "isSelected": NSNull(),
    ]
  }

  private func decode<Value: Decodable>(_ type: Value.Type, object: Any) throws -> Value {
    try JSONDecoder().decode(type, from: JSONSerialization.data(withJSONObject: object))
  }

  private func assertInvalid<Value: Decodable>(
    _ type: Value.Type, object: Any, file: StaticString = #filePath, line: UInt = #line
  ) {
    XCTAssertThrowsError(try decode(type, object: object), file: file, line: line) {
      XCTAssertTrue(
        $0 is DecodingError, "Expected a Codable rejection, received \($0)", file: file, line: line)
    }
  }
}

private struct SnapshotCandidateFixture {
  let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
    "snapshot-candidates-\(UUID().uuidString)", isDirectory: true)

  func store() -> ConfigurationStore {
    ConfigurationStore(
      configuration: .init(directory: directory, owner: "snapshot.candidate.tests"))
  }

  func remove() { try? FileManager.default.removeItem(at: directory) }
}
