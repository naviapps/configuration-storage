import Foundation
import XCTest

@testable import ConfigurationStorage

@MainActor
final class NativeContractTests: XCTestCase {
  func testCurrentHeaderAndBinaryMeetABI14Contract() throws {
    XCTAssertEqual(NativeABI.supportedVersion, 14)
    try NativeABI.requireCompatible()
  }

  func testOlderAndFutureRuntimeABIsAreRejected() {
    for version in [UInt32(0), 13, 15] {
      XCTAssertThrowsError(
        try NativeABI.requireCompatible(headerVersion: 14, runtimeVersion: { version })
      ) { error in
        XCTAssertEqual(error as? ConfigurationStorageError, .init(.unsupportedABIVersion))
      }
    }
  }

  func testStaleHeaderIsRejectedBeforeRuntimeEntryEvenIfItWouldMatchItsBinary() {
    for version in [UInt32(0), 13, 15] {
      var calledRuntime = false
      XCTAssertThrowsError(
        try NativeABI.requireCompatible(
          headerVersion: version,
          runtimeVersion: {
            calledRuntime = true
            return version
          })
      ) { error in
        XCTAssertEqual(error as? ConfigurationStorageError, .init(.unsupportedABIVersion))
      }
      XCTAssertFalse(calledRuntime)
    }
  }

  func testSaveQueueRejectsStaleHeaderBeforeRuntimeOrQueueCreation() {
    for version in [UInt32(0), 13, 15] {
      var calledRuntime = false
      var createdQueue = false
      XCTAssertThrowsError(
        try SnapshotSaveQueue<Int>(
          commit: { _ in },
          headerVersion: version,
          runtimeVersion: {
            calledRuntime = true
            return version
          },
          createQueue: {
            createdQueue = true
            return nil
          })
      ) { error in
        XCTAssertEqual(error as? ConfigurationStorageError, .init(.unsupportedABIVersion))
      }
      XCTAssertFalse(calledRuntime)
      XCTAssertFalse(createdQueue)
    }
  }

  func testSaveQueueRejectsRuntimeABIMismatchBeforeQueueCreation() {
    for version in [UInt32(0), 13, 15] {
      var calledRuntime = false
      var createdQueue = false
      XCTAssertThrowsError(
        try SnapshotSaveQueue<Int>(
          commit: { _ in },
          headerVersion: 14,
          runtimeVersion: {
            calledRuntime = true
            return version
          },
          createQueue: {
            createdQueue = true
            return nil
          })
      ) { error in
        XCTAssertEqual(error as? ConfigurationStorageError, .init(.unsupportedABIVersion))
      }
      XCTAssertTrue(calledRuntime)
      XCTAssertFalse(createdQueue)
    }
  }

  func testSaveQueueReportsNullNativeCreationAsInvalidNativeResponse() {
    var createdQueue = false
    XCTAssertThrowsError(
      try SnapshotSaveQueue<Int>(
        commit: { _ in },
        headerVersion: 14,
        runtimeVersion: { 14 },
        createQueue: {
          createdQueue = true
          return nil
        })
    ) { error in
      XCTAssertEqual(error as? ConfigurationStorageError, .init(.invalidNativeResponse))
    }
    XCTAssertTrue(createdQueue)
  }

  func testBackupRebuildCommandsPreserveOpaqueGenerationAndReviewToken() throws {
    let generation = "opaque-generation/\"\\\n"
    let token = "opaque-review/\"\\\n"
    let prepare = try XCTUnwrap(
      JSONSerialization.jsonObject(
        with: NativeJSON.encode(
          StorageCommand.prepareBackupRebuild(expectedGeneration: generation)))
        as? [String: String])
    XCTAssertEqual(prepare, ["command": "prepareBackupRebuild", "expectedGeneration": generation])
    let rebuild = try XCTUnwrap(
      JSONSerialization.jsonObject(
        with: NativeJSON.encode(
          StorageCommand.rebuildBackup(expectedGeneration: generation, token: token)))
        as? [String: String])
    XCTAssertEqual(
      rebuild, ["command": "rebuildBackup", "expectedGeneration": generation, "token": token])
  }

  func testWriteCommandEncodesOnlyMetadata() throws {
    let payload = Data([0, 255, 128])
    let command = StorageCommand.save(expectedGeneration: "opaque", payload: payload)
    let object = try XCTUnwrap(
      JSONSerialization.jsonObject(with: NativeJSON.encode(command)) as? [String: String])
    XCTAssertEqual(object, ["command": "save", "expectedGeneration": "opaque"])
    XCTAssertEqual(command.payload, payload)
  }

  func testBackupPreparationAndCommitCommandsPreserveOpaqueRotationToken() throws {
    let generation = "opaque-generation/\"\\\n日本語"
    let token = "opaque-rotation/\"\\\n日本語"
    let prepare = try XCTUnwrap(
      JSONSerialization.jsonObject(
        with: NativeJSON.encode(StorageCommand.prepareBackup(expectedGeneration: generation)))
        as? [String: String])
    XCTAssertEqual(prepare, ["command": "prepareBackup", "expectedGeneration": generation])
    let commit = try XCTUnwrap(
      JSONSerialization.jsonObject(
        with: NativeJSON.encode(
          StorageCommand.createBackup(expectedGeneration: generation, token: token)))
        as? [String: String])
    XCTAssertEqual(
      commit, ["command": "createBackup", "expectedGeneration": generation, "token": token])
  }

  func testSnapshotCandidateCommandsEncodeExplicitNullAndPreserveOpaqueIDs() throws {
    let first = try XCTUnwrap(
      JSONSerialization.jsonObject(
        with: NativeJSON.encode(
          StorageCommand.listSnapshotCandidates(limit: 64, after: nil, source: nil)))
        as? [String: Any])
    XCTAssertEqual(Set(first.keys), ["command", "limit", "after", "source"])
    XCTAssertEqual(first["command"] as? String, "listSnapshotCandidates")
    XCTAssertEqual(first["limit"] as? Int, 64)
    XCTAssertTrue(first["after"] is NSNull)
    XCTAssertTrue(first["source"] is NSNull)

    let id = "opaque-candidate/\"\\\n日本語"
    let next = try XCTUnwrap(
      JSONSerialization.jsonObject(
        with: NativeJSON.encode(
          StorageCommand.listSnapshotCandidates(limit: 256, after: id, source: .backup)))
        as? [String: Any])
    XCTAssertEqual(next["command"] as? String, "listSnapshotCandidates")
    XCTAssertEqual(next["limit"] as? Int, 256)
    XCTAssertEqual(next["after"] as? String, id)
    XCTAssertEqual(next["source"] as? String, "backup")
    let read = try XCTUnwrap(
      JSONSerialization.jsonObject(
        with: NativeJSON.encode(StorageCommand.readSnapshotCandidate(id: id)))
        as? [String: String])
    XCTAssertEqual(read, ["command": "readSnapshotCandidate", "id": id])
  }

  func testCancellationDuringRequestEncodingStopsBeforeNativeEntry() async throws {
    let operation = Task {
      var enteredNative = false
      do {
        let _: String = try NativeJSON.dispatch(CancellingRequest()) { _, _, _, _ in
          enteredNative = true
          return nil
        }
        XCTFail("Cancellation while encoding must prevent native dispatch.")
      } catch is CancellationError {}
      return enteredNative
    }
    let enteredNative = try await operation.value
    XCTAssertTrue(operation.isCancelled)
    XCTAssertFalse(enteredNative)
  }

  func testPortableFormatRestoreAndABIFailuresKeepDistinctStableCodes() throws {
    let codes: [ConfigurationStorageError.Code] = [
      .invalidRecoveryCopy, .unsupportedRecoveryVersion, .restoreChanged,
      .unsupportedFormatVersion, .unsupportedABIVersion, .backupChanged,
      .snapshotCandidateMissing, .rawRecoveryLimitExceeded,
    ]
    let decoder = JSONDecoder()
    let encoder = JSONEncoder()
    for code in codes {
      let json = Data("{\"code\":\"\(code.rawValue)\"}".utf8)
      let error = try decoder.decode(ConfigurationStorageError.self, from: json)
      XCTAssertEqual(error.code, code)
      XCTAssertEqual(error.errorDescription, code.rawValue)
      XCTAssertNil(error.recoveryDirectory)
      XCTAssertEqual(
        try decoder.decode(ConfigurationStorageError.self, from: encoder.encode(error)), error)
    }
    XCTAssertEqual(Set(codes.map(\.rawValue)).count, codes.count)
  }
}

private struct CancellingRequest: Encodable {
  func encode(to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode("cancel before dispatch")
    withUnsafeCurrentTask { $0?.cancel() }
  }
}
