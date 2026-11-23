import CConfigurationStorage

/// Client revision space is exhausted; the rejected snapshot remains the host's responsibility.
public enum SnapshotSaveQueueError: Error, Sendable {
  case revisionExhausted
}

/// One coherent observation of client writes, independent of on-disk generation tokens.
public struct SnapshotSaveState: Sendable, Equatable {
  /// Latest accepted client revision.
  public let revision: UInt64
  /// Last successfully committed client revision.
  public let committedRevision: UInt64
  /// Latest coalesced snapshot waiting for a writer.
  public let pendingRevision: UInt64?
  /// Revision whose owned commit is in flight.
  public let writingRevision: UInt64?
  /// Current failed revision retained for explicit retry.
  public let failedRevision: UInt64?
  /// Whether pending or in-flight work remains.
  public let hasUnsavedChanges: Bool
}

/// An opt-in latest-snapshot write queue. Enqueue never writes. The host chooses
/// when to flush, how to debounce/retry, and how to present failures. Snapshots
/// are immutable values; a cancelled flush caller never cancels an owned write.
@MainActor
public final class SnapshotSaveQueue<Snapshot: Sendable> {
  private struct Pending {
    let revision: UInt64
    let snapshot: Snapshot
  }
  private let handle: OpaquePointer
  private let commit: @Sendable (Snapshot) async throws -> Void
  private var pending: Pending?
  private var saving: Task<Void, Error>?

  /// Creates an empty queue without running a timer, callback or disk operation.
  /// Throws if the native ABI is unsupported or queue creation fails.
  public convenience init(commit: @escaping @Sendable (Snapshot) async throws -> Void) throws {
    try self.init(commit: commit, headerVersion: CONFIGURATION_STORAGE_ABI_VERSION)
  }

  init(
    commit: @escaping @Sendable (Snapshot) async throws -> Void,
    headerVersion: UInt32,
    runtimeVersion: () -> UInt32 = { configuration_storage_abi_version() },
    createQueue: () -> OpaquePointer? = { configuration_storage_save_queue_create() }
  ) throws {
    try NativeABI.requireCompatible(headerVersion: headerVersion, runtimeVersion: runtimeVersion)
    guard let handle = createQueue() else {
      throw ConfigurationStorageError(.invalidNativeResponse)
    }
    self.handle = handle
    self.commit = commit
  }

  isolated deinit { configuration_storage_save_queue_destroy(handle) }

  /// Returns a single Rust-owned state snapshot, without advancing write state.
  public var state: SnapshotSaveState {
    let value = configuration_storage_save_queue_view(handle)
    return .init(
      revision: value.revision, committedRevision: value.committed_revision,
      pendingRevision: value.pending_revision == 0 ? nil : value.pending_revision,
      writingRevision: value.writing_revision == 0 ? nil : value.writing_revision,
      failedRevision: value.failed_revision == 0 ? nil : value.failed_revision,
      hasUnsavedChanges: value.has_unsaved_changes != 0)
  }

  /// Replaces the pending snapshot without affecting an in-flight commit.
  /// Revision exhaustion throws before the pending snapshot is replaced.
  @discardableResult
  public func enqueue(_ snapshot: Snapshot) throws -> UInt64 {
    let revision = configuration_storage_save_queue_enqueue(handle)
    guard revision != 0 else { throw SnapshotSaveQueueError.revisionExhausted }
    pending = Pending(revision: revision, snapshot: snapshot)
    return revision
  }

  /// Drains the latest snapshots, sharing one write task between concurrent
  /// callers. A current failure is retained for explicit retry. Older failures
  /// cannot restore obsolete edits over a later pending snapshot.
  public func flush() async throws {
    if let saving { return try await saving.value }
    guard pending != nil else { return }
    let task = Task { @MainActor in
      defer { self.saving = nil }
      while true {
        let revision = configuration_storage_save_queue_begin(self.handle)
        guard revision != 0 else { return }
        guard let next = self.pending, next.revision == revision else {
          preconditionFailure("Native save queue and retained snapshot differ")
        }
        self.pending = nil
        do {
          try await self.commit(next.snapshot)
          precondition(configuration_storage_save_queue_complete(self.handle, revision, 1) == 1)
        } catch {
          let result = configuration_storage_save_queue_complete(self.handle, revision, 0)
          precondition(result == 2 || result == 3, "Native queue rejected a write acknowledgement")
          if result == 2 {
            self.pending = next
            throw error
          }
        }
      }
    }
    saving = task
    try await task.value
  }
}
