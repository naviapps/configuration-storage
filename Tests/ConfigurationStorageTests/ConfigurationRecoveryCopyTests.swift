import CConfigurationStorage
import Foundation
import XCTest

@testable import ConfigurationStorage

@MainActor
final class ConfigurationRecoveryCopyTests: XCTestCase {
  func testEmptyAndAllByteValuesRoundTripWithoutAStoreAndRetainOwnedResults() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    let payloads = [Data(), Data((0...255).map(UInt8.init)), Data(repeating: 0xff, count: 257)]
    var previous: ConfigurationRecoveryCopy?
    var previousPayload: Data?
    for payload in payloads {
      try await ConfigurationRecoveryCopy.write(
        payload: payload, to: fixture.archive, maximumPayloadBytes: 4096)
      let date = Date(timeIntervalSince1970: 1_700_000_000.25)
      try FileManager.default.setAttributes(
        [.modificationDate: date], ofItemAtPath: fixture.archive.path)
      let copy = try await ConfigurationRecoveryCopy.read(
        from: fixture.archive, maximumPayloadBytes: 4096)
      XCTAssertEqual(copy.payload, payload)
      XCTAssertEqual(copy.digest.utf8.count, 64)
      XCTAssertTrue(
        copy.digest.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) })
      XCTAssertEqual(
        copy.modifiedAt.timeIntervalSince1970, date.timeIntervalSince1970, accuracy: 0.001)
      let repeated = try await ConfigurationRecoveryCopy.read(from: fixture.archive)
      XCTAssertEqual(repeated, copy)
      if let previous {
        XCTAssertEqual(previous.payload, previousPayload)
        XCTAssertNotEqual(previous.digest, copy.digest)
      }
      previous = copy
      previousPayload = payload
    }
    XCTAssertEqual(
      try FileManager.default.contentsOfDirectory(atPath: fixture.base.path),
      [fixture.archive.lastPathComponent])
  }

  func testNativePreEpochRecoveryCopyDateProjectsToFoundation() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    let payload = Data([0, 255, 128, 0, 254])
    try await ConfigurationRecoveryCopy.write(payload: payload, to: fixture.archive)
    try FileManager.default.setAttributes(
      [.modificationDate: Date(timeIntervalSince1970: -1.25)], ofItemAtPath: fixture.archive.path)
    let copy = try await ConfigurationRecoveryCopy.read(from: fixture.archive)
    XCTAssertEqual(copy.payload, payload)
    XCTAssertEqual(copy.modifiedAtUnixSeconds, -1.25, accuracy: 0.001)
    XCTAssertEqual(copy.modifiedAt.timeIntervalSince1970, -1.25, accuracy: 0.001)
  }

  func testCopyCrossesStoreOwnersAndSourceReplacementDoesNotChangeReviewedBytes() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    let source = fixture.store(owner: "source.app")
    let target = fixture.store(owner: "target.app")
    let bytes = Data((0...255).map(UInt8.init))
    let sourceSnapshot = try await source.save(expectedGeneration: nil, payload: bytes)
    let targetSnapshot = try await target.save(expectedGeneration: nil, payload: Data([42]))
    try await ConfigurationRecoveryCopy.write(
      payload: XCTUnwrap(sourceSnapshot.payload), to: fixture.archive)
    let reviewed = try await ConfigurationRecoveryCopy.read(from: fixture.archive)
    // The host's validation stands apart from file integrity.
    XCTAssertEqual(reviewed.payload, bytes)
    let token = try await target.prepareRestore(payload: reviewed.payload)

    try await ConfigurationRecoveryCopy.write(payload: Data([99]), to: fixture.archive)
    let replaced = try await ConfigurationRecoveryCopy.read(from: fixture.archive)
    XCTAssertEqual(replaced.payload, Data([99]))
    XCTAssertNotEqual(replaced.digest, reviewed.digest)
    XCTAssertEqual(reviewed.payload, bytes)

    let restored = try await target.restoreSnapshot(payload: reviewed.payload, token: token)
    XCTAssertEqual(restored.payload, bytes)
    XCTAssertNotNil(restored.generation)
    XCTAssertNotEqual(restored.generation, targetSnapshot.generation)
    let reopened = try await fixture.store(owner: "target.app").load()
    XCTAssertEqual(reopened, restored)
    let unchangedSource = try await source.load()
    XCTAssertEqual(unchangedSource, sourceSnapshot)
  }

  func testSwiftDefaultLimitIs16MiB() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    let boundary = Data(repeating: 0xff, count: 16 * 1024 * 1024)
    try await ConfigurationRecoveryCopy.write(payload: boundary, to: fixture.archive)
    let copy = try await ConfigurationRecoveryCopy.read(from: fixture.archive)
    XCTAssertEqual(copy.payload, boundary)
    var oversized = boundary
    oversized.append(0)
    try await expectFailure(.invalidConfiguration) {
      try await ConfigurationRecoveryCopy.write(payload: oversized, to: fixture.archive)
    }
  }

  func testInvalidURLsAndLimitsFailWithoutChangingExistingFiles() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    try await ConfigurationRecoveryCopy.write(payload: Data([1, 2, 3]), to: fixture.archive)
    let original = try Data(contentsOf: fixture.archive)
    let remote = try XCTUnwrap(URL(string: "https://example.invalid/recovery.copy"))
    let embeddedNul = try XCTUnwrap(URL(string: fixture.archive.absoluteString + "%00suffix"))
    for url in [remote, embeddedNul] {
      try await expectFailure(.invalidConfiguration) {
        _ = try await ConfigurationRecoveryCopy.read(from: url)
      }
      try await expectFailure(.invalidConfiguration) {
        try await ConfigurationRecoveryCopy.write(payload: Data(), to: url)
      }
    }
    for limit in [-1, 0, 64 * 1024 * 1024 + 1] {
      try await expectFailure(.invalidConfiguration) {
        _ = try await ConfigurationRecoveryCopy.read(
          from: fixture.archive, maximumPayloadBytes: limit)
      }
      try await expectFailure(.invalidConfiguration) {
        try await ConfigurationRecoveryCopy.write(
          payload: Data(), to: fixture.archive, maximumPayloadBytes: limit)
      }
    }
    XCTAssertEqual(try Data(contentsOf: fixture.archive), original)
  }

  func testTamperedPayloadReturnsNativeIntegrityErrorWithoutChangingSource() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    try await ConfigurationRecoveryCopy.write(
      payload: Data((0...255).map(UInt8.init)), to: fixture.archive)
    var changedPayload = try Data(contentsOf: fixture.archive)
    changedPayload[changedPayload.count - 1] ^= 1
    try changedPayload.write(to: fixture.archive)
    try await expectFailure(.invalidRecoveryCopy) {
      _ = try await ConfigurationRecoveryCopy.read(from: fixture.archive)
    }
    XCTAssertEqual(try Data(contentsOf: fixture.archive), changedPayload)
    XCTAssertEqual(
      try FileManager.default.contentsOfDirectory(atPath: fixture.base.path),
      [fixture.archive.lastPathComponent])
  }

  func testPrecancelledReadAndWriteDoNotTouchFiles() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    try await ConfigurationRecoveryCopy.write(payload: Data([1, 2, 3]), to: fixture.archive)
    let original = try Data(contentsOf: fixture.archive)
    let read = Task {
      withUnsafeCurrentTask { $0?.cancel() }
      return try await ConfigurationRecoveryCopy.read(from: fixture.archive)
    }
    do {
      _ = try await read.value
      XCTFail("A precancelled read must stop before native entry.")
    } catch is CancellationError {}
    let absent = fixture.base.appendingPathComponent("absent.copy")
    for destination in [fixture.archive, absent] {
      let write = Task {
        withUnsafeCurrentTask { $0?.cancel() }
        try await ConfigurationRecoveryCopy.write(payload: Data([99]), to: destination)
      }
      do {
        try await write.value
        XCTFail("A precancelled write must stop before native entry.")
      } catch is CancellationError {}
    }
    XCTAssertFalse(FileManager.default.fileExists(atPath: absent.path))
    XCTAssertEqual(try Data(contentsOf: fixture.archive), original)
  }

  func testConfirmedFileWriteRemainsSuccessfulAfterCallerCancellation() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    let bytes = Data((0...255).map(UInt8.init))
    let write = Task {
      try await ConfigurationRecoveryCopy.write(payload: bytes, to: fixture.archive)
      withUnsafeCurrentTask { $0?.cancel() }
    }
    try await write.value
    XCTAssertTrue(write.isCancelled)
    let copy = try await ConfigurationRecoveryCopy.read(from: fixture.archive)
    XCTAssertEqual(copy.payload, bytes)
  }

  func testCancellationAfterNativeWriteBeforeResponseDecodePreservesConfirmation() async throws {
    let fixture = try RecoveryCopyFixture()
    defer { fixture.remove() }
    let bytes = Data((0...255).map(UInt8.init))
    let write = Task {
      let _: NativeJSON.Null = try NativeJSON.dispatch(
        WriteRequest(path: fixture.archive.path, payload: bytes)
      ) { input, length in
        let result = configuration_storage_recovery_copy_dispatch(input, length)
        // Cancel after actual native I/O, while its owned result still awaits decoding.
        withUnsafeCurrentTask { $0?.cancel() }
        return result
      }
    }
    try await write.value
    XCTAssertTrue(write.isCancelled)
    let copy = try await ConfigurationRecoveryCopy.read(from: fixture.archive)
    XCTAssertEqual(copy.payload, bytes)
  }

  func testCodableUsesBase64AndUnixSecondsAndRejectsMalformedChecksum() throws {
    let digest = String(repeating: "ab", count: 32)
    let decoder = JSONDecoder()
    for seconds in [1_700_000_000.25, -1.25, -1e-9, 2e-9] {
      let json = Data(
        "{\"payload\":\"AP+A\",\"digest\":\"\(digest)\",\"modifiedAt\":\(seconds)}".utf8)
      let copy = try decoder.decode(ConfigurationRecoveryCopy.self, from: json)
      XCTAssertEqual(copy.payload, Data([0, 255, 128]))
      XCTAssertEqual(copy.digest, digest)
      XCTAssertEqual(copy.modifiedAtUnixSeconds, seconds)
      XCTAssertEqual(copy.modifiedAt, Date(timeIntervalSince1970: seconds))
      let encoded = try JSONEncoder().encode(copy)
      let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
      XCTAssertEqual(object["modifiedAt"] as? Double, seconds)
      XCTAssertEqual(try decoder.decode(ConfigurationRecoveryCopy.self, from: encoded), copy)
    }
    for invalid in ["", String(repeating: "a", count: 63), String(repeating: "g", count: 64)] {
      let malformed = Data(
        "{\"payload\":\"\",\"digest\":\"\(invalid)\",\"modifiedAt\":1700000000.25}".utf8)
      XCTAssertThrowsError(try decoder.decode(ConfigurationRecoveryCopy.self, from: malformed))
    }
  }

  private func expectFailure(
    _ code: ConfigurationStorageError.Code, file: StaticString = #filePath, line: UInt = #line,
    operation: () async throws -> Void
  ) async throws {
    do {
      try await operation()
      XCTFail("Expected \(code)", file: file, line: line)
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, code, file: file, line: line)
    }
  }
}

private struct WriteRequest: Encodable {
  let command = "write"
  let path: String
  let maximumPayloadBytes = 4096
  let payload: Data
}

private struct RecoveryCopyFixture {
  let base: URL
  let archive: URL

  init() throws {
    base = FileManager.default.temporaryDirectory.appendingPathComponent(
      "recovery-copy-\(UUID().uuidString)", isDirectory: true)
    archive = base.appendingPathComponent("Reviewed 配置 #1.copy")
    try FileManager.default.createDirectory(at: base, withIntermediateDirectories: false)
  }

  func store(owner: String) -> ConfigurationStore {
    ConfigurationStore(
      configuration: .init(directory: base.appendingPathComponent(owner), owner: owner))
  }

  func remove() {
    try? FileManager.default.removeItem(at: base)
  }
}
