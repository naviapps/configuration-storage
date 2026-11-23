import ConfigurationStorage
import ConfigurationStorageTestSupport
import Foundation
import XCTest

@MainActor
final class BackupRebuildTests: XCTestCase {
  func testNativeBackupRefusalAndRebuildProjectOwnedResults() async throws {
    let fixture = try BackupRebuildFixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let first = try await store.save(expectedGeneration: nil, payload: Data([0, 255, 1]))
    let firstGeneration = try XCTUnwrap(first.generation)
    let preparation = try await store.prepareBackup(expectedGeneration: firstGeneration)
    _ = try await store.createBackup(expectedGeneration: firstGeneration, token: preparation.token)
    let backup = try fixture.native.backupURL
    let latest = try await store.save(
      expectedGeneration: firstGeneration, payload: Data([128, 0, 254]))
    let generation = try XCTUnwrap(latest.generation)
    let damage = Data("damaged checkpoint".utf8)
    try damage.write(to: backup)

    do {
      _ = try await store.prepareBackup(expectedGeneration: generation)
      XCTFail("Normal preparation must refuse a damaged backup.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidStoredData)
      let capture = try XCTUnwrap(error.recoveryDirectory)
      XCTAssertEqual(
        try Data(contentsOf: capture.appendingPathComponent(backup.lastPathComponent)), damage)
    }

    let review = try await store.prepareBackupRebuild(expectedGeneration: generation)
    let rebuilt = try await store.rebuildBackup(expectedGeneration: generation, token: review)
    XCTAssertEqual(rebuilt.snapshot, latest)
    let reopened = try await fixture.store().readBackup()
    XCTAssertEqual(reopened, rebuilt)
    do {
      _ = try await store.rebuildBackup(expectedGeneration: generation, token: review)
      XCTFail("A completed rebuild must report an expired review token.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .backupChanged)
    }
  }

  func testCancelledBackupOperationsDoNotOpenStorage() async throws {
    for operation in ["prepare", "create", "prepareRebuild", "rebuild"] {
      let fixture = try BackupRebuildFixture()
      defer { fixture.remove() }
      let store = fixture.store()
      let operation = Task {
        withUnsafeCurrentTask { $0?.cancel() }
        switch operation {
        case "prepare":
          _ = try await store.prepareBackup(expectedGeneration: "cancelled-generation")
        case "create":
          _ = try await store.createBackup(
            expectedGeneration: "cancelled-generation", token: "rotation")
        case "prepareRebuild":
          _ = try await store.prepareBackupRebuild(expectedGeneration: "cancelled-generation")
        default:
          _ = try await store.rebuildBackup(
            expectedGeneration: "cancelled-generation", token: "review")
        }
      }
      do {
        try await operation.value
        XCTFail("Pre-cancelled backup operations must not enter native storage.")
      } catch is CancellationError {}
      XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))
    }
  }

}

private struct BackupRebuildFixture {
  let directory: URL
  let native: ConfigurationStorageFixture

  init() throws {
    directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    native = ConfigurationStorageFixture(directory: directory, owner: "backup.rebuild.tests")
  }

  func store() -> ConfigurationStore {
    ConfigurationStore(configuration: .init(directory: directory, owner: native.owner))
  }

  func remove() { try? FileManager.default.removeItem(at: directory) }
}
