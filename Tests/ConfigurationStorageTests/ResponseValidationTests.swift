import Foundation
import XCTest

@testable import ConfigurationStorage

@MainActor
final class ResponseValidationTests: XCTestCase {
  func testUninitializedSnapshotRoundTripRequiresExplicitNullKeys() throws {
    let decoder = JSONDecoder()
    let snapshot = try decoder.decode(
      ConfigurationSnapshot.self, from: Data(#"{"generation":null,"payload":null}"#.utf8))
    XCTAssertNil(snapshot.generation)
    XCTAssertNil(snapshot.payload)

    let encoded = try JSONEncoder().encode(snapshot)
    let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
    XCTAssertEqual(Set(object.keys), ["generation", "payload"])
    XCTAssertTrue(object["generation"] is NSNull)
    XCTAssertTrue(object["payload"] is NSNull)
    XCTAssertEqual(try decoder.decode(ConfigurationSnapshot.self, from: encoded), snapshot)
  }

  func testInitializedSnapshotRoundTripsOpaqueGenerationAndEmptyOrBinaryBytes() throws {
    for generation in ["opaque-generation/\"\\\n日本語", " "] {
      for payload in [Data(), Data((0...255).map(UInt8.init))] {
        let json = try JSONSerialization.data(
          withJSONObject: ["generation": generation, "payload": payload.base64EncodedString()])
        let snapshot = try JSONDecoder().decode(ConfigurationSnapshot.self, from: json)
        XCTAssertEqual(snapshot.generation, generation)
        XCTAssertEqual(snapshot.payload, payload)
        let encoded = try JSONEncoder().encode(snapshot)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: String])
        XCTAssertEqual(
          object, ["generation": generation, "payload": payload.base64EncodedString()])
        XCTAssertEqual(
          try JSONDecoder().decode(ConfigurationSnapshot.self, from: encoded), snapshot)
      }
    }
  }

  func testSnapshotRejectsMissingKeysEvenWhenTheOtherValueIsNull() {
    let cases = [
      (#"{}"#, "generation"),
      (#"{"generation":null}"#, "payload"),
      (#"{"payload":null}"#, "generation"),
      (#"{"generation":"opaque"}"#, "payload"),
      (#"{"payload":""}"#, "generation"),
    ]
    for (json, key) in cases {
      XCTAssertThrowsError(
        try JSONDecoder().decode(ConfigurationSnapshot.self, from: Data(json.utf8))
      ) {
        guard case DecodingError.keyNotFound(let missing, _) = $0 else {
          return XCTFail("Expected a missing \(key) key for \(json), received \($0)")
        }
        XCTAssertEqual(missing.stringValue, key)
      }
    }
  }

  func testSnapshotRejectsInconsistentPairsAndEmptyGeneration() {
    for json in [
      #"{"generation":null,"payload":""}"#,
      #"{"generation":null,"payload":"AA=="}"#,
      #"{"generation":"opaque","payload":null}"#,
      #"{"generation":"","payload":""}"#,
      #"{"generation":"","payload":null}"#,
    ] {
      assertInvalid(ConfigurationSnapshot.self, json: json)
    }
  }

  func testSnapshotRejectsWrongTypesAndMalformedBase64() {
    for json in [
      "null", "[]", "0", #""snapshot""#,
      #"{"generation":1,"payload":""}"#,
      #"{"generation":true,"payload":""}"#,
      #"{"generation":{},"payload":""}"#,
      #"{"generation":[],"payload":""}"#,
      #"{"generation":"opaque","payload":1}"#,
      #"{"generation":"opaque","payload":false}"#,
      #"{"generation":"opaque","payload":{}}"#,
      #"{"generation":"opaque","payload":[0,255,128]}"#,
      #"{"generation":"opaque","payload":"not base64!"}"#,
    ] {
      assertInvalid(ConfigurationSnapshot.self, json: json)
    }
  }

  func testSnapshotPayloadLimitCountsDecodedBytesAndIncludesTheBoundary() throws {
    let maximum = ConfigurationStore.Configuration.maximumSupportedPayloadBytes
    for count in [maximum, maximum + 1] {
      try autoreleasepool {
        // Slash-heavy base64 is larger than the payload limit even for valid bytes.
        let json = try JSONSerialization.data(
          withJSONObject: [
            "generation": "opaque-generation",
            "payload": Data(repeating: 0xff, count: count).base64EncodedString(),
          ], options: [.withoutEscapingSlashes])
        if count == maximum {
          let snapshot = try JSONDecoder().decode(ConfigurationSnapshot.self, from: json)
          XCTAssertEqual(snapshot.payload?.count, maximum)
          XCTAssertEqual(snapshot.payload?.first, 0xff)
          XCTAssertEqual(snapshot.payload?.last, 0xff)
        } else {
          XCTAssertThrowsError(try JSONDecoder().decode(ConfigurationSnapshot.self, from: json)) {
            guard case DecodingError.dataCorrupted(let context) = $0 else {
              return XCTFail("Expected oversized payload rejection, received \($0)")
            }
            XCTAssertEqual(context.codingPath.map(\.stringValue), ["payload"])
          }
        }
      }
    }
  }

  func testBackupRoundTripsOpaqueIdentityBytesAndSignedUnixSeconds() throws {
    let generation = "opaque-generation/\"\\\n日本語"
    for token in ["opaque-backup-token/\"\\\n日本語", " "] {
      for payload in [Data(), Data([0, 255, 128, 0, 254])] {
        for seconds in [1_700_000_000.25, 0, -1.25, -11_644_473_600.0, -1e-9, 2e-9] {
          let json = try JSONSerialization.data(
            withJSONObject: [
              "snapshot": ["generation": generation, "payload": payload.base64EncodedString()],
              "token": token,
              "modifiedAt": seconds,
            ])
          let backup = try JSONDecoder().decode(ConfigurationBackup.self, from: json)
          XCTAssertEqual(backup.snapshot.generation, generation)
          XCTAssertEqual(backup.snapshot.payload, payload)
          XCTAssertEqual(backup.token, token)
          XCTAssertEqual(backup.modifiedAtUnixSeconds, seconds)
          XCTAssertEqual(backup.modifiedAt, Date(timeIntervalSince1970: seconds))
          let encoded = try JSONEncoder().encode(backup)
          let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
          XCTAssertEqual(object["modifiedAt"] as? Double, seconds)
          XCTAssertEqual(try JSONDecoder().decode(ConfigurationBackup.self, from: encoded), backup)
        }
      }
    }
  }

  func testBackupRequiresEveryKeyAndRejectsWrongTypes() {
    let snapshot = #"{"generation":"opaque","payload":""}"#
    for json in [
      "null", "[]", "0", #""backup""#, "{}",
      #"{"token":"opaque-token","modifiedAt":0}"#,
      #"{"snapshot":\#(snapshot),"modifiedAt":0}"#,
      #"{"snapshot":\#(snapshot),"token":"opaque-token"}"#,
      #"{"snapshot":null,"token":"opaque-token","modifiedAt":0}"#,
      #"{"snapshot":[],"token":"opaque-token","modifiedAt":0}"#,
      #"{"snapshot":\#(snapshot),"token":null,"modifiedAt":0}"#,
      #"{"snapshot":\#(snapshot),"token":1,"modifiedAt":0}"#,
      #"{"snapshot":\#(snapshot),"token":{},"modifiedAt":0}"#,
      #"{"snapshot":\#(snapshot),"token":"opaque-token","modifiedAt":null}"#,
      #"{"snapshot":\#(snapshot),"token":"opaque-token","modifiedAt":"-1.25"}"#,
      #"{"snapshot":\#(snapshot),"token":"opaque-token","modifiedAt":true}"#,
      #"{"snapshot":\#(snapshot),"token":"opaque-token","modifiedAt":{}}"#,
    ] {
      assertInvalid(ConfigurationBackup.self, json: json)
    }
  }

  func testBackupPreparationRoundTripsExplicitAbsenceAndDistinctOpaqueTokens() throws {
    let token = "opaque-rotation/\"\\\n日本語"
    let backup: [String: Any] = [
      "snapshot": ["generation": "opaque-generation", "payload": "AP+A"],
      "token": "opaque-restore-token", "modifiedAt": -1.25,
    ]
    for observed: Any in [NSNull(), backup] {
      let data = try JSONSerialization.data(withJSONObject: ["backup": observed, "token": token])
      let preparation = try JSONDecoder().decode(ConfigurationBackupPreparation.self, from: data)
      XCTAssertEqual(preparation.token, token)
      if observed is NSNull {
        XCTAssertNil(preparation.backup)
      } else {
        XCTAssertEqual(preparation.backup?.token, "opaque-restore-token")
        XCTAssertEqual(preparation.backup?.snapshot.payload, Data([0, 255, 128]))
        XCTAssertEqual(preparation.backup?.modifiedAt.timeIntervalSince1970, -1.25)
      }
      let encoded = try JSONEncoder().encode(preparation)
      let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
      XCTAssertEqual(Set(object.keys), ["backup", "token"])
      if observed is NSNull { XCTAssertTrue(object["backup"] is NSNull) }
      XCTAssertEqual(
        try JSONDecoder().decode(ConfigurationBackupPreparation.self, from: encoded), preparation)
    }
    let whitespace = try JSONDecoder().decode(
      ConfigurationBackupPreparation.self, from: Data(#"{"backup":null,"token":" "}"#.utf8))
    XCTAssertEqual(whitespace.token, " ")
  }

  func testBackupPreparationRejectsMissingObservationInvalidTokensAndMalformedBackup() {
    for json in [
      "null", "[]", "{}", #"{"token":"rotation"}"#,
      #"{"backup":null}"#, #"{"backup":null,"token":null}"#,
      #"{"backup":null,"token":""}"#, #"{"backup":null,"token":1}"#,
      #"{"backup":null,"token":{}}"#, #"{"backup":false,"token":"rotation"}"#,
      #"{"backup":[],"token":"rotation"}"#, #"{"backup":{},"token":"rotation"}"#,
      #"{"backup":{"snapshot":{"generation":null,"payload":null},"token":"restore","modifiedAt":0},"token":"rotation"}"#,
      #"{"backup":{"snapshot":{"generation":"opaque","payload":"not base64!"},"token":"restore","modifiedAt":0},"token":"rotation"}"#,
    ] {
      assertInvalid(ConfigurationBackupPreparation.self, json: json)
    }
  }

  func testBackupRequiresInitializedValidSnapshotAndNonemptyToken() {
    for snapshot in [
      "{}", #"{"generation":null,"payload":null}"#,
      #"{"generation":"opaque"}"#,
      #"{"generation":null,"payload":""}"#,
      #"{"generation":"opaque","payload":null}"#,
      #"{"generation":"","payload":""}"#,
      #"{"generation":"opaque","payload":"not base64!"}"#,
    ] {
      assertInvalid(
        ConfigurationBackup.self,
        json: #"{"snapshot":\#(snapshot),"token":"opaque-token","modifiedAt":0}"#)
    }
    assertInvalid(
      ConfigurationBackup.self,
      json: #"{"snapshot":{"generation":"opaque","payload":""},"token":"","modifiedAt":0}"#)
  }

  func testBackupRejectsNonfiniteUnixSecondsEvenWhenDecoderAllowsThem() {
    let decoder = JSONDecoder()
    decoder.nonConformingFloatDecodingStrategy = .convertFromString(
      positiveInfinity: "Infinity", negativeInfinity: "-Infinity", nan: "NaN")
    for seconds in [#""Infinity""#, #""-Infinity""#, #""NaN""#] {
      let json = Data(
        #"{"snapshot":{"generation":"opaque","payload":""},"token":"opaque-token","modifiedAt":\#(seconds)}"#
          .utf8)
      XCTAssertThrowsError(try decoder.decode(ConfigurationBackup.self, from: json)) {
        guard case DecodingError.dataCorrupted(let context) = $0 else {
          return XCTFail("Expected nonfinite timestamp rejection, received \($0)")
        }
        XCTAssertEqual(context.codingPath.map(\.stringValue), ["modifiedAt"])
      }
    }
    for seconds in ["1e999", "-1e999"] {
      assertInvalid(
        ConfigurationBackup.self,
        json:
          #"{"snapshot":{"generation":"opaque","payload":""},"token":"opaque-token","modifiedAt":\#(seconds)}"#
      )
    }
  }

  func testNativeDispatchMapsResponseDecodingFailureToInvalidNativeResponse() throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
      "response-validation-\(UUID().uuidString)", isDirectory: true)
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try NativeStore(
      configuration: .init(
        directory: directory, owner: "response.validation", maximumPayloadBytes: 1024))
    let snapshot: ConfigurationSnapshot = try store.dispatch(.load)
    XCTAssertNil(snapshot.generation)
    XCTAssertNil(snapshot.payload)

    // A real successful load result is a snapshot, so decoding it as a backup must fail.
    XCTAssertThrowsError(try store.dispatch(.load) as ConfigurationBackup) {
      XCTAssertEqual($0 as? ConfigurationStorageError, .init(.invalidNativeResponse))
    }
    XCTAssertThrowsError(try store.dispatch(.load) as ConfigurationBackupPreparation) {
      XCTAssertEqual($0 as? ConfigurationStorageError, .init(.invalidNativeResponse))
    }
  }

  private func assertInvalid<Value: Decodable>(
    _ type: Value.Type, json: String, file: StaticString = #filePath, line: UInt = #line
  ) {
    XCTAssertThrowsError(
      try JSONDecoder().decode(type, from: Data(json.utf8)), json, file: file, line: line
    ) {
      XCTAssertTrue(
        $0 is DecodingError, "Expected a Codable rejection, received \($0)", file: file, line: line)
    }
  }
}
