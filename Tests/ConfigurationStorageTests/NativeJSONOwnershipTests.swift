import CConfigurationStorage
import Foundation
import XCTest

@testable import ConfigurationStorage

@MainActor
final class NativeJSONOwnershipTests: XCTestCase {
  func testLargeReplySharesNativeStorageAndRetainsCopiesSlicesAndCopyOnWrite() throws {
    let payload = Data(repeating: 0xa5, count: 1024 * 1024)
    let retained = try autoreleasepool { () throws -> (Data, [UInt8], UInt?) in
      let fixture = try NativeReplyFixture()
      let owner = try fixture.dispatch(.save(expectedGeneration: nil, payload: payload))
      let view = configuration_storage_result_view(owner)
      // Only the address value survives consume; never dereference the borrowed pointer afterward.
      let nativeAddress = view.data.map { UInt(bitPattern: $0) }
      XCTAssertNotNil(nativeAddress)
      XCTAssertEqual(view.code, 0)
      XCTAssertGreaterThan(view.length, payload.count)
      let reply = try NativeJSON.consume(owner)
      XCTAssertEqual(
        reply.withUnsafeBytes { $0.baseAddress.map { UInt(bitPattern: $0) } }, nativeAddress)
      let saved = try decodeNativeSnapshot(reply)
      XCTAssertEqual(saved.payload, payload)

      let copied = reply
      let range = (reply.startIndex + 19)..<(reply.endIndex - 23)
      let slice = reply[range]
      let expectedSlice = Array(slice)
      let sliceAddress = nativeAddress.map { $0 + UInt(range.lowerBound) }
      XCTAssertEqual(
        slice.withUnsafeBytes { $0.baseAddress.map { UInt(bitPattern: $0) } }, sliceAddress)

      let replacement = Data([0, 255, 128, 1])
      let next = try NativeJSON.consume(
        fixture.dispatch(.save(expectedGeneration: saved.generation, payload: replacement)))
      XCTAssertEqual(try decodeNativeSnapshot(next).payload, replacement)
      fixture.close()

      var changed = copied
      changed[changed.startIndex] ^= 0xff
      XCTAssertNotEqual(changed.first, reply.first)
      XCTAssertEqual(copied, reply)
      XCTAssertEqual(try decodeNativeSnapshot(copied), saved)
      XCTAssertEqual(try decodeNativeSnapshot(reply).payload, payload)
      XCTAssertEqual(Array(slice), expectedSlice)
      return (slice, expectedSlice, sliceAddress)
    }
    // The store, original Data, and its full copies are gone; the slice still owns the result.
    XCTAssertEqual(Array(retained.0), retained.1)
    XCTAssertEqual(
      retained.0.withUnsafeBytes { $0.baseAddress.map { UInt(bitPattern: $0) } }, retained.2)
  }

  func testTransferredReplyCanBeReadAndReleasedInDetachedTaskAfterStoreClosure() async throws {
    let payload = Data(repeating: 0x5a, count: 1024 * 1024)
    let decodedPayload = try await Task.detached {
      var reply: Data? = try await MainActor.run {
        let fixture = try NativeReplyFixture()
        let data = try NativeJSON.consume(
          fixture.dispatch(.save(expectedGeneration: nil, payload: payload)))
        fixture.close()
        return data
      }
      let snapshot = try { () throws -> ConfigurationSnapshot in
        try decodeNativeSnapshot(XCTUnwrap(reply))
      }()
      // No source Data is retained by the creating actor or returned from this task.
      reply = nil
      return snapshot.payload
    }.value
    XCTAssertEqual(decodedPayload, payload)
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
      request, maximumPayloadBytes: 3, using: configuration_storage_recovery_copy_dispatch)
    XCTAssertEqual(copy.payload, payload)
    XCTAssertThrowsError(
      try NativeJSON.dispatch(
        request, maximumPayloadBytes: 2, using: configuration_storage_recovery_copy_dispatch)
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

private func decodeNativeSnapshot(_ data: Data) throws -> ConfigurationSnapshot {
  let decoder = JSONDecoder()
  decoder.dataDecodingStrategy = .base64
  return try decoder.decode(ConfigurationSnapshot.self, from: data)
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
      try XCTUnwrap(
        configuration_storage_store_dispatch(
          store, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count))
    }
  }

  func decode<Value: Decodable>(_ command: StorageCommand, limit: Int, as _: Value.Type) throws
    -> Value
  {
    let store = try XCTUnwrap(handle)
    return try NativeJSON.dispatch(command, maximumPayloadBytes: limit) { bytes, count in
      configuration_storage_store_dispatch(store, bytes, count)
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
