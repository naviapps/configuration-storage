import Foundation

/// `nil` payload and generation identify an uninitialized store; empty `Data` is initialized data.
public struct ConfigurationSnapshot: Codable, Equatable, Sendable {
  public let generation: String?
  public let payload: Data?

  private enum CodingKeys: String, CodingKey {
    case generation, payload
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    // Both keys are required even when their values are null.
    generation = try container.decode(String?.self, forKey: .generation)
    payload = try container.decode(Data?.self, forKey: .payload)
    guard (generation == nil) == (payload == nil), generation?.isEmpty != true else {
      throw DecodingError.dataCorruptedError(
        forKey: .generation, in: container,
        debugDescription:
          "Expected null generation and payload, or a nonempty generation with payload bytes")
    }
    if let payload, payload.count > NativeJSON.payloadLimit(from: decoder) {
      throw DecodingError.dataCorruptedError(
        forKey: .payload, in: container,
        debugDescription: "Payload exceeds the supported byte limit")
    }
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(generation, forKey: .generation)
    try container.encode(payload, forKey: .payload)
  }
}

/// An integrity-checked backup with an opaque identity, before host schema validation.
/// The token also binds the current raw selection and expires when that selection changes.
public struct ConfigurationBackup: Codable, Equatable, Sendable {
  public let snapshot: ConfigurationSnapshot
  public let token: String
  /// File modification time in signed Unix seconds, with the engine's fractional precision.
  public let modifiedAtUnixSeconds: Double
  /// The time as a Foundation date, suitable for presentation.
  public var modifiedAt: Date { Date(timeIntervalSince1970: modifiedAtUnixSeconds) }

  private enum CodingKeys: String, CodingKey {
    case snapshot, token, modifiedAt
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    snapshot = try container.decode(ConfigurationSnapshot.self, forKey: .snapshot)
    guard snapshot.generation != nil else {
      throw DecodingError.dataCorruptedError(
        forKey: .snapshot, in: container,
        debugDescription: "Expected an initialized backup snapshot")
    }
    token = try container.decode(String.self, forKey: .token)
    guard !token.isEmpty else {
      throw DecodingError.dataCorruptedError(
        forKey: .token, in: container, debugDescription: "Expected a nonempty opaque backup token")
    }
    modifiedAtUnixSeconds = try container.decodeFiniteUnixSeconds(forKey: .modifiedAt)
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(snapshot, forKey: .snapshot)
    try container.encode(token, forKey: .token)
    try container.encode(modifiedAtUnixSeconds, forKey: .modifiedAt)
  }
}

/// A held backup observation, or explicit absence, and an opaque backup-rotation token.
/// Validate any existing backup's payload before passing `token` to `createBackup`.
public struct ConfigurationBackupPreparation: Codable, Equatable, Sendable {
  /// The integrity-checked selected backup, before host schema validation, or `nil` if absent.
  public let backup: ConfigurationBackup?
  /// Authorizes rotation only while the reviewed raw primary and backup identities still match.
  /// This token is distinct from `backup.token`, which belongs to backup restoration.
  public let token: String

  private enum CodingKeys: String, CodingKey {
    case backup, token
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    // An explicit null is an observed absence; a missing key is not that observation.
    backup = try container.decode(ConfigurationBackup?.self, forKey: .backup)
    token = try container.decode(String.self, forKey: .token)
    guard !token.isEmpty else {
      throw DecodingError.dataCorruptedError(
        forKey: .token, in: container,
        debugDescription: "Expected a nonempty opaque backup-rotation token")
    }
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(backup, forKey: .backup)
    try container.encode(token, forKey: .token)
  }
}

/// Stable native failure codes and an optional directory containing raw recovery evidence.
public struct ConfigurationStorageError: Error, Codable, Equatable, Sendable, LocalizedError {
  public enum Code: String, Codable, Sendable {
    case invalidConfiguration, ownerMismatch, invalidStoredData
    /// The configured root was absent when opening an existing store.
    case storeMissing
    /// The stored SQLite format version is unsupported, independent of the framework ABI.
    case unsupportedFormatVersion
    case conflict, busy, notInitialized, backupMissing, backupChanged, ioFailure
    /// A portable recovery copy failed structural or integrity validation.
    case invalidRecoveryCopy
    /// The portable recovery copy version is unsupported, independent of stored SQLite and ABI.
    case unsupportedRecoveryVersion
    /// Caller payload bytes or the current raw selection changed after restore preparation.
    case restoreChanged
    /// A managed snapshot candidate no longer exists.
    case snapshotCandidateMissing
    /// The raw candidate family exceeds the bounded inspection budget.
    case rawRecoveryLimitExceeded
    /// The facade could not decode an owned native result.
    case invalidNativeResponse
    /// The framework's ABI differs from the facade's supported ABI.
    case unsupportedABIVersion
  }

  public let code: Code
  public let recoveryDirectory: URL?

  public init(_ code: Code, recoveryDirectory: URL? = nil) {
    self.code = code
    self.recoveryDirectory = recoveryDirectory
  }

  public var errorDescription: String? { code.rawValue }

  private enum CodingKeys: String, CodingKey {
    case code, recoveryDirectory
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    code = try container.decode(Code.self, forKey: .code)
    recoveryDirectory = try container.decodeIfPresent(String.self, forKey: .recoveryDirectory)
      .map { URL(fileURLWithPath: $0, isDirectory: true) }
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(code, forKey: .code)
    try container.encodeIfPresent(recoveryDirectory?.path, forKey: .recoveryDirectory)
  }
}

extension KeyedDecodingContainer {
  func decodeFiniteUnixSeconds(forKey key: Key) throws -> Double {
    let seconds = try decode(Double.self, forKey: key)
    guard seconds.isFinite else {
      throw DecodingError.dataCorruptedError(
        forKey: key, in: self, debugDescription: "Expected finite Unix seconds")
    }
    return seconds
  }
}
