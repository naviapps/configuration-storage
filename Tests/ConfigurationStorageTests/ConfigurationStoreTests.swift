import ConfigurationStorage
import ConfigurationStorageTestSupport
import Foundation
import XCTest

@MainActor
final class ConfigurationStoreTests: XCTestCase {
  func testStoredFormatAndABIVersionFailuresRemainDistinct() throws {
    let decoder = JSONDecoder()
    let stored = try decoder.decode(
      ConfigurationStorageError.self, from: Data(#"{"code":"unsupportedFormatVersion"}"#.utf8))
    XCTAssertEqual(stored.code, .unsupportedFormatVersion)
    let abi = ConfigurationStorageError(.unsupportedABIVersion)
    XCTAssertNotEqual(stored, abi)
    XCTAssertEqual(stored.errorDescription, "unsupportedFormatVersion")
    XCTAssertEqual(abi.errorDescription, "unsupportedABIVersion")
    XCTAssertEqual(
      try decoder.decode(ConfigurationStorageError.self, from: JSONEncoder().encode(stored)), stored
    )
  }

  func testInitializationDoesNotTouchDiskAndLoadDistinguishesUninitializedFromEmptyData()
    async throws
  {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))

    let uninitialized = try await store.load()
    XCTAssertNil(uninitialized.generation)
    XCTAssertNil(uninitialized.payload)

    let empty = try await store.save(expectedGeneration: nil, payload: Data())
    XCTAssertNotNil(empty.generation)
    XCTAssertEqual(empty.payload, Data())
    XCTAssertNotEqual(empty, uninitialized)
    let loaded = try await store.load()
    XCTAssertEqual(loaded, empty)
  }

  func testExplicitExistingAdmissionProjectsMissingOwnerAndSelectionErrors() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let configuration = fixture.configuration
    do {
      _ = try await ConfigurationStore.openExisting(configuration: configuration)
      XCTFail("Explicit existing admission must report a missing root before returning.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .storeMissing)
    }
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))

    _ = try await ConfigurationStore.open(configuration: configuration)
    let existing = try await ConfigurationStore.openExisting(configuration: configuration)
    let initial = try await existing.load()
    XCTAssertNil(initial.generation)
    XCTAssertNil(initial.payload)
    let payload = Data([0, 255, 42])
    let saved = try await existing.save(expectedGeneration: nil, payload: payload)
    let reopened = try await ConfigurationStore.openExisting(configuration: configuration)
    let loaded = try await reopened.load()
    XCTAssertEqual(loaded, saved)
    XCTAssertEqual(loaded.payload, payload)

    let foreign = ConfigurationStore.Configuration(
      directory: fixture.directory, owner: "com.example.foreign")
    do {
      _ = try await ConfigurationStore.openExisting(configuration: foreign)
      XCTFail("Explicit existing admission must reject a different owner.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .ownerMismatch)
    }

    let selection = fixture.directory.appendingPathComponent("active-generation")
    try Data([255, 0, 47]).write(to: selection)
    let damaged = try await ConfigurationStore.openExisting(configuration: configuration)
    do {
      _ = try await damaged.load()
      XCTFail("An admitted root can still have an invalid snapshot selection.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidStoredData)
    }
  }

  func testCancelledExplicitOpenDoesNotCreateTheStore() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let configuration = fixture.configuration
    let operation = Task {
      withUnsafeCurrentTask { $0?.cancel() }
      return try await ConfigurationStore.open(configuration: configuration)
    }
    do {
      _ = try await operation.value
      XCTFail("Cancelled admission entered native code.")
    } catch is CancellationError {}
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))
  }

  func testOpeningExistingLeavesMissingParentsUntouchedAndRetriesAfterCreation() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let directory = fixture.directory.appendingPathComponent("nested", isDirectory: true)
    let configuration = ConfigurationStore.Configuration(directory: directory, owner: "fixture")
    let store = ConfigurationStore(openingExisting: configuration)
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))

    do {
      _ = try await store.load()
      XCTFail("Opening an existing store must not create its root or parents.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .storeMissing)
    }
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))

    let creator = ConfigurationStore(configuration: configuration)
    let initial = try await creator.load()
    let admitted = try await store.load()
    XCTAssertEqual(admitted, initial)
    XCTAssertNil(admitted.generation)
    XCTAssertNil(admitted.payload)

    let saved = try await store.save(expectedGeneration: nil, payload: Data([0, 255, 128]))
    let reloaded = try await ConfigurationStore(openingExisting: configuration).load()
    XCTAssertNotNil(saved.generation)
    XCTAssertEqual(reloaded, saved)
  }

  func testOpaqueBytesRoundTripAfterReopeningWithoutSchemaOrTextConversion() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let bytes = Data([0, 255, 128, 0, 123, 34, 1, 254])
    let saved = try await store.save(expectedGeneration: nil, payload: bytes)
    let reopened = fixture.store()
    let loaded = try await reopened.load()
    XCTAssertEqual(loaded, saved)
    XCTAssertEqual(loaded.payload, bytes)
    let unchanged = try await reopened.save(expectedGeneration: loaded.generation, payload: bytes)
    XCTAssertEqual(unchanged.generation, saved.generation)
  }

  func testOpaquePayloadAtConfiguredLimitCanUpdateAndReopen() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let configuration = ConfigurationStore.Configuration(
      directory: fixture.directory, owner: "com.example.fixture",
      maximumPayloadBytes: 36 * 1024 * 1024)
    let store = ConfigurationStore(configuration: configuration)
    let original = try await store.save(expectedGeneration: nil, payload: Data([0]))
    let bytes = Data(repeating: 0xff, count: 36 * 1024 * 1024)
    let saved = try await store.save(expectedGeneration: original.generation, payload: bytes)
    XCTAssertNotEqual(saved.generation, original.generation)
    XCTAssertEqual(saved.payload, bytes)
    let reopened = ConfigurationStore(configuration: configuration)
    let loaded = try await reopened.load()
    XCTAssertEqual(loaded, saved)
    XCTAssertEqual(loaded.payload, bytes)
  }

  func testLoadDoesNotAuthorizeAStaleWriter() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let writer = fixture.store()
    let competitor = fixture.store()
    let original = try await writer.save(expectedGeneration: nil, payload: Data("original".utf8))
    let newer = try await competitor.save(
      expectedGeneration: original.generation, payload: Data("newer".utf8))
    let observed = try await writer.load()
    XCTAssertEqual(observed, newer)

    do {
      _ = try await writer.save(
        expectedGeneration: original.generation, payload: Data("stale".utf8))
      XCTFail("Observation must not replace the caller's explicit write generation.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .conflict)
    }
    do {
      _ = try await writer.save(expectedGeneration: nil, payload: Data("replacement".utf8))
      XCTFail("An initialized store must reject an initialization write.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .conflict)
    }
    let retained = try await competitor.load()
    XCTAssertEqual(retained, newer)
  }

  func testParentAliasCreatesNestedStoreAndOpenedHandleSurvivesRetargeting() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let manager = FileManager.default
    let original = fixture.base.appendingPathComponent("original", isDirectory: true)
    let other = fixture.base.appendingPathComponent("other", isDirectory: true)
    for directory in [original, other] {
      try manager.createDirectory(at: directory, withIntermediateDirectories: false)
    }
    let alias = fixture.base.appendingPathComponent("alias", isDirectory: true)
    try manager.createSymbolicLink(at: alias, withDestinationURL: original)
    let nested = "new-parent/ConfigurationStorage"
    let supplied = alias.appendingPathComponent(nested, isDirectory: true)
    let store = ConfigurationStore(configuration: .init(directory: supplied, owner: "alias-test"))
    XCTAssertFalse(manager.fileExists(atPath: supplied.path))
    let uninitialized = try await store.load()
    XCTAssertNil(uninitialized.payload)
    let saved = try await store.save(expectedGeneration: nil, payload: Data([0, 255, 128]))
    let reopened = ConfigurationStore(
      configuration: .init(directory: supplied, owner: "alias-test"))
    let loaded = try await reopened.load()
    XCTAssertEqual(loaded, saved)

    try manager.removeItem(at: alias)
    try manager.createSymbolicLink(at: alias, withDestinationURL: other)
    let updated = try await store.save(
      expectedGeneration: saved.generation, payload: Data("original updated".utf8))
    let retained = try await reopened.load()
    XCTAssertEqual(retained, updated)
    XCTAssertFalse(manager.fileExists(atPath: supplied.path))
    let direct = ConfigurationStore(
      configuration: .init(
        directory: original.appendingPathComponent(nested, isDirectory: true), owner: "alias-test"))
    let directLoaded = try await direct.load()
    XCTAssertEqual(directLoaded, updated)
    let afterRetarget = ConfigurationStore(
      configuration: .init(directory: supplied, owner: "alias-test"))
    let otherLoaded = try await afterRetarget.load()
    XCTAssertNil(otherLoaded.payload)
  }

  func testRootAliasRemainsRejectedWithoutCreatingManagedFiles() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let alias = fixture.base.appendingPathComponent("root-alias", isDirectory: true)
    try FileManager.default.createDirectory(
      at: fixture.directory, withIntermediateDirectories: false)
    try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: fixture.directory)
    let store = ConfigurationStore(configuration: .init(directory: alias, owner: "alias-test"))
    do {
      _ = try await store.load()
      XCTFail("A parent alias is supported, but the managed root itself must not be an alias.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidStoredData)
    }
    XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: fixture.directory.path), [])
  }

  func testNativeBackupRoundTripAndTypedRotationRejection() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let saved = try await store.save(expectedGeneration: nil, payload: Data([0, 255, 128]))
    let generation = try XCTUnwrap(saved.generation)
    let absent = try await store.prepareBackup(expectedGeneration: generation)
    XCTAssertNil(absent.backup)
    let backup = try await store.createBackup(expectedGeneration: generation, token: absent.token)
    XCTAssertEqual(backup.snapshot, saved)

    let review = try await store.prepareBackup(expectedGeneration: generation)
    XCTAssertEqual(review.backup, backup)
    XCTAssertNotEqual(review.token, backup.token)
    do {
      _ = try await store.createBackup(expectedGeneration: generation, token: backup.token)
      XCTFail("The nested restore token must not authorize rotation.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .backupChanged)
    }
    let retained = try await store.readBackup()
    let current = try await store.load()
    XCTAssertEqual(retained, backup)
    XCTAssertEqual(current, saved)
  }

  func testNativeBackupProjectsPreEpochModificationTime() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let original = try await store.save(
      expectedGeneration: nil, payload: Data([0, 255, 128, 0, 254]))
    let generation = try XCTUnwrap(original.generation)
    let backupPreparation = try await store.prepareBackup(expectedGeneration: generation)
    _ = try await store.createBackup(expectedGeneration: generation, token: backupPreparation.token)
    let native = ConfigurationStorageFixture(
      directory: fixture.directory, owner: store.configuration.owner)
    let path = try native.backupURL
    let date = Date(timeIntervalSince1970: -1.25)
    try FileManager.default.setAttributes([.modificationDate: date], ofItemAtPath: path.path)

    let candidate = try await store.readBackup()
    XCTAssertEqual(candidate.snapshot, original)
    XCTAssertFalse(candidate.token.isEmpty)
    XCTAssertEqual(candidate.modifiedAt.timeIntervalSince1970, -1.25, accuracy: 0.001)
    let reusePreparation = try await store.prepareBackup(expectedGeneration: generation)
    XCTAssertEqual(reusePreparation.backup, candidate)
    let reused = try await store.createBackup(
      expectedGeneration: generation, token: reusePreparation.token)
    XCTAssertEqual(reused, candidate)
  }

  func testRestoreBackupProjectsStaleTokenAndRestoredSnapshot() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let payload = Data([0, 255, 128])
    let original = try await store.save(expectedGeneration: nil, payload: payload)
    let generation = try XCTUnwrap(original.generation)
    let preparation = try await store.prepareBackup(expectedGeneration: generation)
    _ = try await store.createBackup(expectedGeneration: generation, token: preparation.token)
    let reviewed = try await store.readBackup()
    XCTAssertEqual(reviewed.snapshot, original)

    let writer = fixture.store()
    let newer = try await writer.save(
      expectedGeneration: generation, payload: Data("newer write".utf8))
    do {
      _ = try await store.restoreBackup(token: reviewed.token)
      XCTFail("A stale review token must fail.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .backupChanged)
    }
    let retained = try await store.load()
    XCTAssertEqual(retained, newer)

    let refreshed = try await store.readBackup()
    let restored = try await store.restoreBackup(token: refreshed.token)
    XCTAssertEqual(restored.payload, payload)
    XCTAssertNotNil(restored.generation)
    XCTAssertNotEqual(restored.generation, newer.generation)
    let reopened = try await fixture.store().load()
    XCTAssertEqual(reopened, restored)
  }

  func testCorruptPrimaryRemainsAvailableAfterRecoveryAndRestore() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let original = try await store.save(expectedGeneration: nil, payload: Data("recoverable".utf8))
    let primary = try XCTUnwrap(fixture.databaseBytes().keys.first)
    let backupPreparation = try await store.prepareBackup(
      expectedGeneration: XCTUnwrap(original.generation))
    _ = try await store.createBackup(
      expectedGeneration: XCTUnwrap(original.generation), token: backupPreparation.token)
    let corrupt = Data("retained corrupt database".utf8)
    try corrupt.write(to: primary)

    do {
      _ = try await store.load()
      XCTFail("Corrupt storage must not become an empty snapshot.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidStoredData)
      let recovery = try XCTUnwrap(error.recoveryDirectory)
      XCTAssertTrue(recovery.isFileURL)
      XCTAssertEqual(
        try Data(contentsOf: recovery.appendingPathComponent(primary.lastPathComponent)), corrupt)
    }
    let candidate = try await store.readBackup()
    XCTAssertEqual(candidate.snapshot.payload, original.payload)
    let restored = try await store.restoreBackup(token: candidate.token)
    XCTAssertEqual(restored.payload, original.payload)
    XCTAssertEqual(try Data(contentsOf: primary), corrupt)
  }

  func testRestoreProjectsChangedTokenErrorAndReturnsReviewedBytes() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let reviewed = Data([0, 255, 128, 42])
    let token = try await store.prepareRestore(payload: reviewed)
    XCTAssertFalse(token.isEmpty)
    let before = try await store.load()
    XCTAssertNil(before.generation)

    var changed = reviewed
    changed[0] ^= 1
    do {
      _ = try await store.restoreSnapshot(payload: changed, token: token)
      XCTFail("The public restore call accepted bytes different from the reviewed payload.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .restoreChanged)
    }
    let unchanged = try await store.load()
    XCTAssertEqual(unchanged, before)
    let restored = try await store.restoreSnapshot(payload: reviewed, token: token)
    XCTAssertEqual(restored.payload, reviewed)
    XCTAssertNotNil(restored.generation)
    let reopened = try await fixture.store().load()
    XCTAssertEqual(reopened, restored)
  }

  func testRestorePreparationEnforcesPayloadLimitWithoutChangingSelection() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let original = try await store.save(expectedGeneration: nil, payload: Data([1]))
    let existing = try fixture.databaseBytes()
    do {
      _ = try await store.prepareRestore(payload: Data(repeating: 0, count: 4097))
      XCTFail("Host-provided restore bytes must respect the store's configured limit.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidConfiguration)
    }
    XCTAssertEqual(try fixture.databaseBytes(), existing)
    let observed = try await store.load()
    XCTAssertEqual(observed, original)
    let empty = Data()
    let token = try await store.prepareRestore(payload: empty)
    let restored = try await store.restoreSnapshot(payload: empty, token: token)
    XCTAssertEqual(restored.payload, empty)
    XCTAssertNotNil(restored.generation)
  }

  func testOversizedWriteRequestsLeaveTheLazyStoreUnopened() async throws {
    let requests: [(ConfigurationStore, Data) async throws -> Void] = [
      { _ = try await $0.save(expectedGeneration: nil, payload: $1) },
      { _ = try await $0.prepareRestore(payload: $1) },
      { _ = try await $0.restoreSnapshot(payload: $1, token: "unused") },
    ]
    for request in requests {
      let fixture = try Fixture()
      defer { fixture.remove() }
      let store = ConfigurationStore(
        configuration: .init(
          directory: fixture.directory, owner: fixture.configuration.owner, maximumPayloadBytes: 1))
      do {
        try await request(store, Data([0, 255]))
        XCTFail("The configured payload limit must be enforced.")
      } catch let error as ConfigurationStorageError {
        XCTAssertEqual(error.code, .invalidConfiguration)
      }
      XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))
    }
  }

  func testPrecancelledPreparationAndRestoreCannotInitializeOrReplaceSelectedStorage() async throws
  {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let bytes = Data("reviewed".utf8)
    let preparation = Task {
      withUnsafeCurrentTask { $0?.cancel() }
      return try await store.prepareRestore(payload: bytes)
    }
    do {
      _ = try await preparation.value
      XCTFail("Cancelled preparation must not open a native store.")
    } catch is CancellationError {}
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))

    let original = try await store.save(expectedGeneration: nil, payload: Data("original".utf8))
    let token = try await store.prepareRestore(payload: bytes)
    let existing = try fixture.databaseBytes()
    let operation = Task {
      withUnsafeCurrentTask { $0?.cancel() }
      return try await store.restoreSnapshot(payload: bytes, token: token)
    }
    do {
      _ = try await operation.value
      XCTFail("Cancelled restoration must not replace the selected generation.")
    } catch is CancellationError {}
    XCTAssertEqual(try fixture.databaseBytes(), existing)
    let observed = try await store.load()
    XCTAssertEqual(observed, original)

    let confirmed = Task {
      let restored = try await store.restoreSnapshot(payload: bytes, token: token)
      withUnsafeCurrentTask { $0?.cancel() }
      return restored
    }
    let restored = try await confirmed.value
    XCTAssertTrue(confirmed.isCancelled)
    XCTAssertEqual(restored.payload, bytes)
    let reopened = try await fixture.store().load()
    XCTAssertEqual(reopened, restored)
  }

  func testMissingDatabaseAndSelectionRejectInitializationUntilVerifiedBackupRestore() async throws
  {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let native = ConfigurationStorageFixture(
      directory: fixture.directory, owner: store.configuration.owner)
    let original = try await store.save(
      expectedGeneration: nil, payload: Data([0, 255, 128, 0, 123, 34, 1, 254]))
    let backupPreparation = try await store.prepareBackup(
      expectedGeneration: XCTUnwrap(original.generation))
    let backup = try await store.createBackup(
      expectedGeneration: XCTUnwrap(original.generation), token: backupPreparation.token)
    let primary = try native.databaseURL()
    let backupBytes = try Data(contentsOf: native.backupURL)
    try FileManager.default.removeItem(at: primary)
    try FileManager.default.removeItem(at: native.selectionURL)
    let retained = try fixture.databaseBytes()
    XCTAssertEqual(retained.count, 1)
    XCTAssertEqual(retained.values.first, backupBytes)
    XCTAssertTrue(
      try FileManager.default.contentsOfDirectory(atPath: primary.deletingLastPathComponent().path)
        .isEmpty)

    let reopened = fixture.store()
    do {
      _ = try await reopened.load()
      XCTFail("A retained backup must not be mistaken for an uninitialized store.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidStoredData)
    }
    do {
      _ = try await reopened.save(expectedGeneration: nil, payload: Data("replacement seed".utf8))
      XCTFail("Missing selected data must not authorize an initialization write.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidStoredData)
    }
    XCTAssertFalse(FileManager.default.fileExists(atPath: native.selectionURL.path))
    XCTAssertFalse(FileManager.default.fileExists(atPath: primary.path))
    XCTAssertEqual(try fixture.databaseBytes(), retained)

    let candidate = try await reopened.readBackup()
    XCTAssertEqual(candidate.snapshot, original)
    XCTAssertEqual(candidate.modifiedAt, backup.modifiedAt)
    let restored = try await reopened.restoreBackup(token: candidate.token)
    XCTAssertEqual(restored.payload, original.payload)
    XCTAssertNotNil(restored.generation)
    XCTAssertNotEqual(restored.generation, original.generation)
    XCTAssertNotEqual(try native.databaseURL(), primary)
    let reloaded = try await fixture.store().load()
    XCTAssertEqual(reloaded, restored)
    XCTAssertFalse(FileManager.default.fileExists(atPath: primary.path))
    XCTAssertEqual(try Data(contentsOf: native.backupURL), backupBytes)
    let retainedBackup = try await reopened.readBackup()
    XCTAssertEqual(retainedBackup.snapshot, original)
    XCTAssertEqual(retainedBackup.modifiedAt, backup.modifiedAt)
  }

  func testMissingBackupAndWrongOwnerReturnStableFailuresWithoutReplacingData() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    do {
      _ = try await store.readBackup()
      XCTFail("A missing backup cannot be presented as an initialized snapshot.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .backupMissing)
    }
    let saved = try await store.save(expectedGeneration: nil, payload: Data("retained".utf8))
    let existing = try fixture.databaseBytes()
    let foreign = ConfigurationStore(
      configuration: .init(directory: fixture.directory, owner: "com.example.other"))
    do {
      _ = try await foreign.load()
      XCTFail("A different owner must not claim an established store.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .ownerMismatch)
    }
    XCTAssertEqual(try fixture.databaseBytes(), existing)
    let retained = try await store.load()
    XCTAssertEqual(retained, saved)
  }

  func testInvalidLimitFailsLazilyWithoutCreatingStorage() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = ConfigurationStore(
      configuration: .init(
        directory: fixture.directory, owner: "com.example.fixture", maximumPayloadBytes: 0))
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))
    do {
      _ = try await store.load()
      XCTFail("The native engine must reject invalid limits.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidConfiguration)
    }
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))
  }

  func testCancelledTaskDoesNotInitializeOrSave() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let operation = Task {
      try await store.save(expectedGeneration: nil, payload: Data("cancelled".utf8))
    }
    operation.cancel()
    do {
      _ = try await operation.value
      XCTFail("Cancellation before native entry must stop the write.")
    } catch is CancellationError {}
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.path))
    let observed = try await store.load()
    XCTAssertNil(observed.payload)
  }

  func testConfirmedWriteIsRetainedAfterCallerCancellation() async throws {
    let fixture = try Fixture()
    defer { fixture.remove() }
    let store = fixture.store()
    let operation = Task {
      let committed = try await store.save(expectedGeneration: nil, payload: Data("confirmed".utf8))
      withUnsafeCurrentTask { $0?.cancel() }
      return committed
    }
    let confirmed = try await operation.value
    XCTAssertTrue(operation.isCancelled)
    let loaded = try await fixture.store().load()
    XCTAssertEqual(loaded, confirmed)
  }

  func testCodablePreservesNullEmptyAndBinaryPayloadsAndSignedUnixBackupDates() throws {
    let decoder = JSONDecoder()
    let uninitialized = try decoder.decode(
      ConfigurationSnapshot.self, from: Data(#"{"generation":null,"payload":null}"#.utf8))
    let empty = try decoder.decode(
      ConfigurationSnapshot.self,
      from: Data(#"{"generation":"opaque-generation","payload":""}"#.utf8))
    let binary = try decoder.decode(
      ConfigurationSnapshot.self,
      from: Data(#"{"generation":"opaque-generation","payload":"AP+A"}"#.utf8))
    XCTAssertNil(uninitialized.payload)
    XCTAssertEqual(empty.payload, Data())
    XCTAssertEqual(binary.payload, Data([0, 255, 128]))
    XCTAssertEqual(
      try decoder.decode(ConfigurationSnapshot.self, from: JSONEncoder().encode(binary)), binary)
    for seconds in [1_700_000_000.25, -1.25] {
      let backup = try decoder.decode(
        ConfigurationBackup.self,
        from: Data(
          #"{"snapshot":{"generation":"opaque","payload":""},"token":"identity","modifiedAt":\#(seconds)}"#
            .utf8))
      XCTAssertEqual(backup.modifiedAt.timeIntervalSince1970, seconds)
      XCTAssertEqual(
        try decoder.decode(ConfigurationBackup.self, from: JSONEncoder().encode(backup)), backup)
    }
  }
}

private struct Fixture {
  let base: URL
  let directory: URL

  init() throws {
    base = FileManager.default.temporaryDirectory.appendingPathComponent(
      UUID().uuidString, isDirectory: true)
    directory = base.appendingPathComponent("store", isDirectory: true)
    try FileManager.default.createDirectory(at: base, withIntermediateDirectories: false)
  }

  var configuration: ConfigurationStore.Configuration {
    .init(directory: directory, owner: "com.example.fixture", maximumPayloadBytes: 4096)
  }

  func store() -> ConfigurationStore {
    ConfigurationStore(configuration: configuration)
  }

  func databaseBytes() throws -> [URL: Data] {
    guard let files = FileManager.default.enumerator(at: directory, includingPropertiesForKeys: nil)
    else {
      return [:]
    }
    var result: [URL: Data] = [:]
    for case let url as URL in files where url.pathExtension == "sqlite" {
      result[url] = try Data(contentsOf: url)
    }
    return result
  }

  func remove() {
    try? FileManager.default.removeItem(at: base)
  }
}
