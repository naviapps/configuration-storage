import ConfigurationStorage
import Darwin
import Foundation

private struct AcceptanceFailure: Error {
  let message: String
}

private func require(_ condition: Bool, _ message: String) throws {
  guard condition else { throw AcceptanceFailure(message: message) }
}

@main
struct ConfigurationStorageConsumer {
  static func main() async throws {
    let manager = FileManager.default
    let directory = manager.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    try manager.createDirectory(at: directory, withIntermediateDirectories: false)
    defer { try? manager.removeItem(at: directory) }
    try await exercise(directory: directory.appendingPathComponent("store", isDirectory: true))
    try await exerciseRecoveryCopy(in: directory)
    try await exerciseSaveQueue()
    print("Standalone ConfigurationStorage acceptance passed.")
  }

  private static func exercise(directory: URL) async throws {
    let configuration = ConfigurationStore.Configuration(
      directory: directory, owner: "com.example.standalone-storage")
    let writer = try await ConfigurationStore.open(configuration: configuration)
    let initial = try await writer.load()
    try require(initial.generation == nil && initial.payload == nil, "The owned store is not empty")

    let bytes = Data([0, 255, 128, 0])
    let original = try await writer.save(expectedGeneration: nil, payload: bytes)
    guard let generation = original.generation else {
      throw AcceptanceFailure(message: "The native save did not return a generation")
    }
    let reopened = try await ConfigurationStore.openExisting(configuration: configuration)
    let loaded = try await reopened.load()
    try require(
      loaded == original && loaded.payload == bytes, "Native round trip changed the bytes")

    let preparation = try await writer.prepareBackup(expectedGeneration: generation)
    let backup = try await writer.createBackup(
      expectedGeneration: generation, token: preparation.token)
    let observedBackup = try await reopened.readBackup()
    try require(observedBackup == backup, "Equal native backup observations compared unequal")
    let metadata = try installLegacyCheckpoint(in: directory)
    let legacyBackup = try await reopened.readBackup()
    try require(
      legacyBackup.snapshot == backup.snapshot, "Desktop metadata obscured a legacy backup")

    let page = try await writer.listSnapshotCandidates()
    let observedPage = try await reopened.listSnapshotCandidates()
    try require(observedPage == page, "Equal candidate pages compared unequal")
    guard let candidateID = page.candidates.first(where: { $0.isSelected == true })?.id else {
      throw AcceptanceFailure(message: "The native listing did not include a selected candidate")
    }
    let candidate = try await writer.readSnapshotCandidate(id: candidateID)
    let observedCandidate = try await reopened.readSnapshotCandidate(id: candidateID)
    try require(observedCandidate == candidate, "Equal candidate observations compared unequal")

    let newer = try await reopened.save(
      expectedGeneration: generation, payload: Data("{}".utf8))
    let observed = try await writer.load()
    try require(observed == newer, "The writer did not observe the newer generation")
    do {
      _ = try await writer.save(expectedGeneration: generation, payload: Data([2]))
      throw AcceptanceFailure(message: "A stale generation replaced the newer snapshot")
    } catch let error as ConfigurationStorageError {
      try require(error.code == .conflict, "Stale rejection lost its public conflict error")
    }
    let retained = try await reopened.load()
    try require(retained == newer, "A rejected stale write changed the stored snapshot")

    guard let newerGeneration = newer.generation,
      let oldBackup = page.candidates.first(where: { $0.source == .backup && $0.isSelected == true }
      )
    else {
      throw AcceptanceFailure(message: "The checkpoint fixture could not prepare reconstruction")
    }
    let rebuild = try await reopened.prepareBackupRebuild(expectedGeneration: newerGeneration)
    let rebuilt = try await reopened.rebuildBackup(
      expectedGeneration: newerGeneration, token: rebuild)
    let previous = try await reopened.readSnapshotCandidate(id: oldBackup.id)
    try require(
      rebuilt.snapshot == newer && previous.snapshot == backup.snapshot
        && previous.isSelected == false,
      "Explicit reconstruction did not retain the earlier opaque checkpoint")
    let retainedMetadata = try Data(contentsOf: metadata)
    try require(retainedMetadata == Data([4, 5, 6]), "Recovery changed desktop metadata")
  }

  private static func installLegacyCheckpoint(in directory: URL) throws -> URL {
    let marker = directory.appendingPathComponent("active-backup")
    let selected = try String(contentsOf: marker, encoding: .utf8)
    let backups = directory.appendingPathComponent("Backups", isDirectory: true)
    try FileManager.default.moveItem(
      at: backups.appendingPathComponent(selected + ".sqlite"),
      to: backups.appendingPathComponent("snapshot.sqlite"))
    try FileManager.default.removeItem(at: marker)
    let metadata = backups.appendingPathComponent(".DS_Store")
    try Data([4, 5, 6]).write(to: metadata)
    return metadata
  }

  private static func exerciseRecoveryCopy(in directory: URL) async throws {
    let destination = directory.appendingPathComponent("portable.copy")
    let bytes = Data([0, 255, 128])
    let oversized = bytes + Data([1])
    try await ConfigurationRecoveryCopy.write(
      payload: bytes, to: destination, maximumPayloadBytes: bytes.count)

    do {
      try await ConfigurationRecoveryCopy.write(
        payload: oversized, to: destination, maximumPayloadBytes: bytes.count)
      throw AcceptanceFailure(message: "Oversized portable write was accepted")
    } catch let error as ConfigurationStorageError {
      try require(error.code == .invalidConfiguration, "Incorrect portable write error")
    }

    let cancelled = Task {
      withUnsafeCurrentTask { $0?.cancel() }
      try await ConfigurationRecoveryCopy.write(
        payload: oversized, to: destination, maximumPayloadBytes: bytes.count)
    }
    do {
      try await cancelled.value
      throw AcceptanceFailure(message: "Pre-cancelled portable write was accepted")
    } catch is CancellationError {}

    guard let qualified = URL(string: destination.absoluteString + "?revision=old#review") else {
      throw AcceptanceFailure(message: "Could not construct a file URL with qualifiers")
    }
    do {
      try await ConfigurationRecoveryCopy.write(
        payload: Data([1]), to: qualified, maximumPayloadBytes: bytes.count)
      throw AcceptanceFailure(message: "A qualified file URL replaced the bare recovery path")
    } catch let error as ConfigurationStorageError {
      try require(error.code == .invalidConfiguration, "Incorrect qualified file URL error")
    }

    var earlier: ConfigurationRecoveryCopy?
    for (time, expectedSeconds) in [
      (timespec(tv_sec: -1, tv_nsec: 999_999_999), -1e-9),
      (timespec(tv_sec: 0, tv_nsec: 2), 2e-9),
    ] {
      let result = destination.path.withCString {
        utimensat(AT_FDCWD, $0, [time, time], 0)
      }
      try require(result == 0, "Could not set the portable file's exact Unix timestamp")
      let observed = try await ConfigurationRecoveryCopy.read(from: destination)
      try require(observed.modifiedAtUnixSeconds == expectedSeconds, "The SDK lost native time")
      let encoded = try JSONEncoder().encode(observed)
      let fields = try JSONSerialization.jsonObject(with: encoded) as? [String: Any]
      try require(
        fields?["modifiedAt"] as? Double == expectedSeconds,
        "Codable lost native fractional Unix seconds")
      if let earlier {
        try require(earlier != observed, "Distinct native file times compared equal")
      }
      earlier = observed
    }

    let signedDate = Date(timeIntervalSince1970: -1.25)
    try FileManager.default.setAttributes(
      [.modificationDate: signedDate], ofItemAtPath: destination.path)
    let reviewed = try await ConfigurationRecoveryCopy.read(
      from: destination, maximumPayloadBytes: bytes.count)
    try require(reviewed.payload == bytes, "Rejected portable write changed the retained copy")
    try require(reviewed.modifiedAt == signedDate, "The copy lost its signed fractional date")
    let repeated = try await ConfigurationRecoveryCopy.read(
      from: destination, maximumPayloadBytes: bytes.count)
    try require(repeated == reviewed, "Equal portable copies compared unequal")

    let configuration = ConfigurationStore.Configuration(
      directory: directory.appendingPathComponent("portable-restore", isDirectory: true),
      owner: "com.example.portable-restore")
    let store = try await ConfigurationStore.open(configuration: configuration)
    let original = try await store.save(expectedGeneration: nil, payload: Data([77]))
    let originalPage = try await store.listSnapshotCandidates(source: .generation)
    guard let originalID = originalPage.candidates.first?.id else {
      throw AcceptanceFailure(message: "The original generation was not discoverable")
    }
    let token = try await store.prepareRestore(payload: reviewed.payload)
    let replacement = Data([1, 254, 127])
    try await ConfigurationRecoveryCopy.write(payload: replacement, to: destination)
    let restored = try await restoreAfterFailedCapture(
      store: store, directory: configuration.directory, original: original,
      payload: reviewed.payload, token: token)
    let reopened = try await ConfigurationStore.openExisting(configuration: configuration)
    let loaded = try await reopened.load()
    let originalCandidate = try await reopened.readSnapshotCandidate(id: originalID)
    try require(
      restored.generation != nil && loaded == restored && loaded.payload == bytes,
      "Restoration did not persist the reviewed payload")
    try require(
      originalCandidate.snapshot == original && originalCandidate.isSelected == false,
      "Restoration did not retain the original generation")
    let retained = try await ConfigurationRecoveryCopy.read(from: destination)
    try require(
      retained.payload == replacement && retained.digest != reviewed.digest,
      "Restoration changed the replacement portable copy")
  }

  private static func restoreAfterFailedCapture(
    store: ConfigurationStore, directory: URL, original: ConfigurationSnapshot,
    payload: Data, token: String
  ) async throws -> ConfigurationSnapshot {
    let obstruction = directory.appendingPathComponent("Recovery")
    let sentinel = Data("unavailable recovery directory".utf8)
    try sentinel.write(to: obstruction)
    do {
      _ = try await store.restoreSnapshot(payload: payload, token: token)
      throw AcceptanceFailure(
        message: "Restoration published before preserving the original selector")
    } catch let error as ConfigurationStorageError {
      try require(
        error.code == .invalidStoredData, "Incorrect failure when selector capture was blocked")
    }
    let current = try await store.load()
    let blocker = try Data(contentsOf: obstruction)
    try require(
      current == original && blocker == sentinel, "Failed restore changed the original data")
    try FileManager.default.removeItem(at: obstruction)
    return try await store.restoreSnapshot(payload: payload, token: token)
  }

  @MainActor
  private static func exerciseSaveQueue() async throws {
    let queue = try SnapshotSaveQueue<Int> { _ in }
    let idle = queue.state
    try require(idle == queue.state, "Equal idle save queue observations compared unequal")

    let revision = try queue.enqueue(1)
    let pending = queue.state
    try require(
      pending == queue.state && pending != idle && pending.pendingRevision == revision
        && pending.hasUnsavedChanges,
      "Save queue observations did not reflect pending work")

    try await queue.flush()
    let committed = queue.state
    try require(
      committed == queue.state && committed != pending && committed.committedRevision == revision
        && !committed.hasUnsavedChanges,
      "Save queue observations did not reflect the committed revision")
  }
}
