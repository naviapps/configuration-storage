import Foundation

/// Serializes native disk operations without choosing a payload schema or save schedule.
public actor ConfigurationStore {
  /// Immutable host-selected ownership, location and resource limits.
  public struct Configuration: Equatable, Sendable {
    /// Maximum decoded snapshot size supported by the native engine.
    public static let maximumSupportedPayloadBytes = 64 * 1024 * 1024
    public let directory: URL
    public let owner: String
    public let maximumPayloadBytes: Int

    /// Stores values only. The first operation validates them and opens the native store.
    public init(directory: URL, owner: String, maximumPayloadBytes: Int = 16 * 1024 * 1024) {
      self.directory = directory
      self.owner = owner
      self.maximumPayloadBytes = maximumPayloadBytes
    }
  }

  public nonisolated let configuration: Configuration
  private let admission: NativeStore.Admission
  private var nativeStore: NativeStore?

  /// Lazily opens or creates a store. Initialization performs no I/O.
  public init(configuration: Configuration) {
    self.configuration = configuration
    admission = .createOrOpen
  }

  /// Lazily opens an existing store owned by this configuration. Initialization performs no I/O.
  /// Operations never create a missing root, claim an owner, or create a missing root lock.
  /// A missing root at open fails with `storeMissing`. An owned store may still be uninitialized.
  public init(openingExisting configuration: Configuration) {
    self.configuration = configuration
    admission = .existingOnly
  }

  /// Opens or claims the configured store before returning, without reading its snapshot.
  @concurrent
  public static func open(configuration: Configuration) async throws -> ConfigurationStore {
    try ConfigurationStore(opening: configuration, admission: .createOrOpen)
  }

  /// Checks an existing store's ownership and lock before returning, without reading its snapshot.
  /// The root must exist, and neither admission nor subsequent operations create a missing lock.
  @concurrent
  public static func openExisting(configuration: Configuration) async throws -> ConfigurationStore {
    try ConfigurationStore(opening: configuration, admission: .existingOnly)
  }

  private init(opening configuration: Configuration, admission: NativeStore.Admission) throws {
    try Task.checkCancellation()
    self.configuration = configuration
    self.admission = admission
    nativeStore = try NativeStore(configuration: configuration, admission: admission)
  }

  /// Observes the current snapshot without remembering or advancing a writer baseline.
  public func load() throws -> ConfigurationSnapshot {
    try perform(.load)
  }

  /// Lists unverified generation and backup references, including known selected files.
  /// The limit must be in 1...256; IDs and cursors are opaque managed file identities.
  /// Pass `nextCursor` unchanged to continue; restart without a cursor to rescan concurrent edits.
  /// Supply a source to scan only that location. Use a cursor from the same source.
  public func listSnapshotCandidates(
    limit: Int = 64, after: String? = nil, source: ConfigurationSnapshotCandidateSource? = nil
  ) throws
    -> ConfigurationSnapshotCandidatePage
  {
    try perform(.listSnapshotCandidates(limit: limit, after: after, source: source))
  }

  /// Reads verified immutable bytes from a private copy of a managed candidate's SQLite family.
  /// The host must validate payload semantics before using the existing snapshot restore flow.
  /// This observation neither selects a generation nor advances an editing baseline.
  public func readSnapshotCandidate(id: String) throws -> ConfigurationSnapshotCandidate {
    try perform(.readSnapshotCandidate(id: id))
  }

  /// Atomically saves opaque bytes if the supplied generation still matches.
  /// Pass `nil` only to initialize an uninitialized store, and retain the returned generation.
  public func save(expectedGeneration: String?, payload: Data) throws -> ConfigurationSnapshot {
    try perform(.save(expectedGeneration: expectedGeneration, payload: payload))
  }

  /// Holds the selected backup, or its absence, and both raw identities under the root lease.
  /// The host must validate any returned backup payload before approving its replacement.
  /// A stale primary generation fails with `conflict`; preparation never rotates a backup.
  public func prepareBackup(expectedGeneration: String) throws -> ConfigurationBackupPreparation {
    try perform(.prepareBackup(expectedGeneration: expectedGeneration))
  }

  /// Creates or reuses a backup only while the prepared primary and backup identities still match.
  /// Pass the rotation token from `prepareBackup`, not the nested backup's restore token.
  /// Changed raw state or a token for another operation fails with `backupChanged`, even on reuse.
  public func createBackup(expectedGeneration: String, token: String) throws -> ConfigurationBackup
  {
    try perform(.createBackup(expectedGeneration: expectedGeneration, token: token))
  }

  /// Reviews the complete raw primary and backup identities for an explicit backup rebuild.
  /// Requires the current primary generation; preparation does not select a new backup.
  /// A stale expected generation fails with `conflict`.
  /// Known valid backups with a foreign owner or unsupported format are refused.
  public func prepareBackupRebuild(expectedGeneration: String) async throws -> String {
    try perform(.prepareBackupRebuild(expectedGeneration: expectedGeneration))
  }

  /// Rechecks the reviewed identities and selects a fresh backup, retaining the prior family.
  /// A stale token fails with `backupChanged`, including after a newer primary write or any
  /// raw family change, and requires a new review. This explicitly permits replacing an invalid
  /// backup selection; normal `createBackup(expectedGeneration:token:)` remains conservative.
  public func rebuildBackup(expectedGeneration: String, token: String) async throws
    -> ConfigurationBackup
  {
    try perform(.rebuildBackup(expectedGeneration: expectedGeneration, token: token))
  }

  /// Returns backup bytes and a token binding the backup plus current raw selection for host validation.
  public func readBackup() throws -> ConfigurationBackup {
    try perform(.readBackup)
  }

  /// Rechecks the validated backup and raw selection, then selects a fresh, retained generation.
  /// The host must validate the payload returned by `readBackup()` before passing its token.
  /// Any backup or raw selection change requires reading and validating a new candidate.
  public func restoreBackup(token: String) throws -> ConfigurationSnapshot {
    try perform(.restoreBackup(token: token))
  }

  /// Binds caller-owned payload bytes and the current raw selection under the root lease.
  /// The host must validate payload semantics before restoring. Preparation never advances
  /// the caller's writer baseline.
  public func prepareRestore(payload: Data) async throws -> String {
    try perform(.prepareRestore(payload: payload))
  }

  /// Selects a fresh generation containing exactly the host-validated payload, retaining originals.
  /// Use the same immutable bytes passed to `prepareRestore(payload:)`. Changed payload or raw
  /// selection requires a new token and host review. Existing writer baselines remain stale.
  public func restoreSnapshot(payload: Data, token: String) async throws -> ConfigurationSnapshot {
    try perform(.restoreSnapshot(payload: payload, token: token))
  }

  private func perform<Value: Decodable>(_ command: StorageCommand) throws -> Value {
    try Task.checkCancellation()
    guard !command.exceedsPayloadLimit(configuration.maximumPayloadBytes) else {
      throw ConfigurationStorageError(.invalidConfiguration)
    }
    let store: NativeStore
    if let nativeStore {
      store = nativeStore
    } else {
      store = try NativeStore(configuration: configuration, admission: admission)
      nativeStore = store
    }
    // No suspension or cancellation check after native entry: a confirmed write stays confirmed.
    return try store.dispatch(command)
  }
}
