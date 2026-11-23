import CConfigurationStorage
import Foundation
import XCTest

@testable import ConfigurationStorage

@MainActor
final class NativeJSONOwnershipTests: XCTestCase {
  func testLargeReplyKeepsValueSemanticsAfterReleasingNativeOwner() throws {
    let payload = Data(repeating: 0xa5, count: 1024 * 1024)
    let retained = try autoreleasepool { () throws -> (Data, [UInt8]) in
      let fixture = try NativeReplyFixture()
      let owner = try fixture.dispatch(.save(expectedGeneration: nil, payload: payload))
      let view = configuration_storage_result_view_v14(owner)
      XCTAssertNotNil(view.payload)
      XCTAssertEqual(view.code, 0)
      XCTAssertLessThan(view.length, 1024)
      XCTAssertEqual(view.payload_length, payload.count)
      let reply = try NativeJSON.consume(owner)
      let bytes = try XCTUnwrap(reply.payload)
      let saved = try reply.decode(ConfigurationSnapshot.self)
      XCTAssertEqual(saved.payload, payload)

      let copied = bytes
      let range = (bytes.startIndex + 19)..<(bytes.endIndex - 23)
      let slice = bytes[range]
      let expectedSlice = Array(slice)

      let replacement = Data([0, 255, 128, 1])
      let next = try NativeJSON.consume(
        fixture.dispatch(.save(expectedGeneration: saved.generation, payload: replacement)))
      XCTAssertEqual(try next.decode(ConfigurationSnapshot.self).payload, replacement)
      fixture.close()

      var changed = copied
      changed[changed.startIndex] ^= 0xff
      XCTAssertNotEqual(changed.first, bytes.first)
      XCTAssertEqual(copied, bytes)
      XCTAssertEqual(saved.payload, payload)
      XCTAssertEqual(Array(slice), expectedSlice)
      return (slice, expectedSlice)
    }
    XCTAssertEqual(Array(retained.0), retained.1)
  }

  func testTransferredReplyCanBeReadAndReleasedInDetachedTaskAfterStoreClosure() async throws {
    let payload = Data(repeating: 0x5a, count: 1024 * 1024)
    let decodedPayload = try await Task.detached {
      var reply: NativeJSON.Reply? = try await MainActor.run {
        let fixture = try NativeReplyFixture()
        let data = try NativeJSON.consume(
          fixture.dispatch(.save(expectedGeneration: nil, payload: payload)))
        fixture.close()
        return data
      }
      let snapshot = try XCTUnwrap(reply).decode(ConfigurationSnapshot.self)
      // No source Data is retained by the creating actor or returned from this task.
      reply = nil
      return snapshot.payload
    }.value
    XCTAssertEqual(decodedPayload, payload)
  }

  func testAbsentAndEmptyPayloadsStayDistinctInNativeReplies() throws {
    let fixture = try NativeReplyFixture()
    let missing = try NativeJSON.consume(fixture.dispatch(.load))
    XCTAssertNil(missing.payload)
    XCTAssertNil(try missing.decode(ConfigurationSnapshot.self).generation)

    let empty = try NativeJSON.consume(
      fixture.dispatch(.save(expectedGeneration: nil, payload: Data())))
    XCTAssertEqual(empty.payload, Data())
    let snapshot = try empty.decode(ConfigurationSnapshot.self)
    XCTAssertNotNil(snapshot.generation)
    XCTAssertEqual(snapshot.payload, Data())
  }

  func testMetadataAndPayloadDisagreementIsRejected() {
    let cases: [(String, Data?)] = [
      (#"{"generation":null}"#, Data()),
      (#"{"generation":"opaque"}"#, nil),
      (#"{"generation":"opaque","payload":""}"#, Data()),
    ]
    for (metadata, payload) in cases {
      let reply = NativeJSON.Reply(metadata: Data(metadata.utf8), payload: payload)
      XCTAssertThrowsError(try reply.decode(ConfigurationSnapshot.self)) {
        XCTAssertEqual($0 as? ConfigurationStorageError, .init(.invalidNativeResponse))
      }
    }
    let reply = NativeJSON.Reply(metadata: Data(#""opaque-token""#.utf8), payload: Data())
    XCTAssertThrowsError(try reply.decode(String.self)) {
      XCTAssertEqual($0 as? ConfigurationStorageError, .init(.invalidNativeResponse))
    }
  }

  func testConfiguredReplyLimitAppliesToSnapshotsNestedBackupsAndCandidates() throws {
    let fixture = try NativeReplyFixture()
    let payload = Data([1, 2, 3])
    let saved = try fixture.decode(
      .save(expectedGeneration: nil, payload: payload), limit: 3, as: ConfigurationSnapshot.self)
    let generation = try XCTUnwrap(saved.generation)
    let preparation = try fixture.decode(
      .prepareBackup(expectedGeneration: generation), limit: 3,
      as: ConfigurationBackupPreparation.self)
    let backup = try fixture.decode(
      .createBackup(expectedGeneration: generation, token: preparation.token), limit: 3,
      as: ConfigurationBackup.self)
    let candidates = try fixture.decode(
      .listSnapshotCandidates(limit: 64, after: nil, source: .generation), limit: 3,
      as: ConfigurationSnapshotCandidatePage.self)
    let candidateID = try XCTUnwrap(candidates.candidates.first?.id)

    try assertReplyLimit(fixture, .load, as: ConfigurationSnapshot.self)
    try assertReplyLimit(fixture, .readBackup, as: ConfigurationBackup.self)
    try assertReplyLimit(
      fixture, .prepareBackup(expectedGeneration: generation),
      as: ConfigurationBackupPreparation.self)
    try assertReplyLimit(
      fixture, .readSnapshotCandidate(id: candidateID), as: ConfigurationSnapshotCandidate.self)
    XCTAssertEqual(saved.payload, payload)
    XCTAssertEqual(backup.snapshot.payload, payload)
  }

  func testConfiguredReplyLimitAppliesToPortableCopies() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
      "configuration-copy-limit-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
    defer { try? FileManager.default.removeItem(at: directory) }
    let file = directory.appendingPathComponent("portable.copy")
    let payload = Data([1, 2, 3])
    try await ConfigurationRecoveryCopy.write(payload: payload, to: file)

    // Simulate a native library that returns bytes above the host's requested limit.
    struct ReadRequest: Encodable {
      let command = "read"
      let path: String
      let maximumPayloadBytes = 3
    }
    let request = ReadRequest(path: file.path)
    let copy: ConfigurationRecoveryCopy = try NativeJSON.dispatch(
      request, maximumPayloadBytes: 3, using: configuration_storage_recovery_copy_request)
    XCTAssertEqual(copy.payload, payload)
    XCTAssertThrowsError(
      try NativeJSON.dispatch(
        request, maximumPayloadBytes: 2, using: configuration_storage_recovery_copy_request)
        as ConfigurationRecoveryCopy
    ) {
      XCTAssertEqual($0 as? ConfigurationStorageError, .init(.invalidNativeResponse))
    }
  }

  private func assertReplyLimit<Value: Decodable>(
    _ fixture: NativeReplyFixture, _ command: StorageCommand, as type: Value.Type
  ) throws {
    _ = try fixture.decode(command, limit: 3, as: type)
    XCTAssertThrowsError(try fixture.decode(command, limit: 2, as: type)) {
      XCTAssertEqual($0 as? ConfigurationStorageError, .init(.invalidNativeResponse))
    }
  }
}

/// Opens only a unique temporary store and transfers each result to NativeJSON.consume once.
private final class NativeReplyFixture {
  private let directory: URL
  private var handle: OpaquePointer?

  init() throws {
    directory = FileManager.default.temporaryDirectory.appendingPathComponent(
      "native-reply-ownership-\(UUID().uuidString)", isDirectory: true)
    try NativeABI.requireCompatible()
    let input = try NativeJSON.encode(
      OpenConfiguration(directory: directory.path, owner: "com.example.reply-ownership"))
    var failure: OpaquePointer?
    let opened = input.withUnsafeBytes { bytes in
      configuration_storage_store_open(
        bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count, &failure)
    }
    if let failure {
      if let opened { configuration_storage_store_destroy(opened) }
      _ = try NativeJSON.consume(failure)
      throw ConfigurationStorageError(.invalidNativeResponse)
    }
    handle = try XCTUnwrap(opened)
  }

  deinit {
    close()
    try? FileManager.default.removeItem(at: directory)
  }

  func dispatch(_ command: StorageCommand) throws -> OpaquePointer {
    let store = try XCTUnwrap(handle)
    let input = try NativeJSON.encode(command)
    return try input.withUnsafeBytes { bytes in
      let metadata = bytes.bindMemory(to: UInt8.self)
      if let payload = command.payload, !payload.isEmpty {
        return try payload.withUnsafeBytes { bytes in
          let payload = bytes.bindMemory(to: UInt8.self)
          return try XCTUnwrap(
            configuration_storage_store_request(
              store, metadata.baseAddress, metadata.count, payload.baseAddress, payload.count))
        }
      }
      return try XCTUnwrap(
        configuration_storage_store_request(store, metadata.baseAddress, metadata.count, nil, 0))
    }
  }

  func decode<Value: Decodable>(_ command: StorageCommand, limit: Int, as _: Value.Type) throws
    -> Value
  {
    let store = try XCTUnwrap(handle)
    return try NativeJSON.dispatch(command, payload: command.payload, maximumPayloadBytes: limit) {
      metadata, length, payload, payloadLength in
      configuration_storage_store_request(store, metadata, length, payload, payloadLength)
    }
  }

  func close() {
    if let handle {
      configuration_storage_store_destroy(handle)
      self.handle = nil
    }
  }

  private struct OpenConfiguration: Encodable {
    let directory: String
    let owner: String
    let maximumPayloadBytes = 16 * 1024 * 1024
  }
}
