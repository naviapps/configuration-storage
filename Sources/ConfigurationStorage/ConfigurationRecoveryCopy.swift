import CConfigurationStorage
import Foundation

/// An independent portable copy of opaque host bytes, without app or store-owner identity.
/// The checksum verifies integrity, not authenticity. Hosts must validate payload semantics.
public struct ConfigurationRecoveryCopy: Codable, Equatable, Sendable {
  public let payload: Data
  public let digest: String
  /// File modification time in signed Unix seconds, with the engine's fractional precision.
  public let modifiedAtUnixSeconds: Double
  /// The time as a Foundation date, suitable for presentation.
  public var modifiedAt: Date { Date(timeIntervalSince1970: modifiedAtUnixSeconds) }

  /// Reads a bounded file without opening a store. Later source-file edits cannot change
  /// these immutable bytes; pass the reviewed payload itself when preparing and restoring.
  @concurrent
  public static func read(
    from url: URL, maximumPayloadBytes: Int = 16 * 1024 * 1024
  ) async throws -> Self {
    try Task.checkCancellation()
    let path = try NativeFilePath.local(url)
    try validateInput(maximumPayloadBytes: maximumPayloadBytes)
    return try NativeJSON.dispatch(
      RecoveryCopyCommand(
        command: "read", path: path, maximumPayloadBytes: maximumPayloadBytes, payload: nil),
      maximumPayloadBytes: maximumPayloadBytes,
      using: configuration_storage_recovery_copy_dispatch)
  }

  /// Atomically writes opaque bytes to a host-selected file outside live managed storage.
  /// Rejects oversized input before encoding the request.
  /// The parent directory must exist. Cancellation before native entry prevents the write;
  /// a confirmed write remains successful even if the caller is subsequently cancelled.
  @concurrent
  public static func write(
    payload: Data, to url: URL, maximumPayloadBytes: Int = 16 * 1024 * 1024
  ) async throws {
    try Task.checkCancellation()
    let path = try NativeFilePath.local(url)
    try validateInput(maximumPayloadBytes: maximumPayloadBytes, payloadBytes: payload.count)
    let _: NativeJSON.Null = try NativeJSON.dispatch(
      RecoveryCopyCommand(
        command: "write", path: path, maximumPayloadBytes: maximumPayloadBytes, payload: payload
      ),
      using: configuration_storage_recovery_copy_dispatch)
  }

  private static func validateInput(maximumPayloadBytes: Int, payloadBytes: Int = 0) throws {
    guard
      (1...ConfigurationStore.Configuration.maximumSupportedPayloadBytes)
        .contains(maximumPayloadBytes), payloadBytes <= maximumPayloadBytes
    else { throw ConfigurationStorageError(.invalidConfiguration) }
  }

  private enum CodingKeys: String, CodingKey {
    case payload, digest, modifiedAt
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    payload = try container.decode(Data.self, forKey: .payload)
    guard payload.count <= NativeJSON.payloadLimit(from: decoder) else {
      throw DecodingError.dataCorruptedError(
        forKey: .payload, in: container,
        debugDescription: "Payload exceeds the supported byte limit")
    }
    digest = try container.decode(String.self, forKey: .digest)
    guard digest.utf8.count == 64,
      digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) })
    else {
      throw DecodingError.dataCorruptedError(
        forKey: .digest, in: container, debugDescription: "Expected a lowercase hex64 checksum")
    }
    modifiedAtUnixSeconds = try container.decodeFiniteUnixSeconds(forKey: .modifiedAt)
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(payload, forKey: .payload)
    try container.encode(digest, forKey: .digest)
    try container.encode(modifiedAtUnixSeconds, forKey: .modifiedAt)
  }
}

private struct RecoveryCopyCommand: Encodable {
  let command: String
  let path: String
  let maximumPayloadBytes: Int
  let payload: Data?
}
