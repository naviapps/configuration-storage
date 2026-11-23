import Foundation

/// The managed location of a candidate.
public enum ConfigurationSnapshotCandidateSource: String, Codable, Sendable {
  case generation
  case backup
}

/// An unverified managed file reference; listing does not validate its payload or select it.
public struct ConfigurationSnapshotCandidateReference: Codable, Equatable, Sendable {
  /// An opaque file identity, distinct from a snapshot's content-bound write generation.
  public let id: String
  public let source: ConfigurationSnapshotCandidateSource
  /// `nil` means selection cannot be determined for this source.
  public let isSelected: Bool?

  private enum CodingKeys: String, CodingKey {
    case id, source, isSelected
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    id = try container.decode(String.self, forKey: .id)
    source = try container.decode(ConfigurationSnapshotCandidateSource.self, forKey: .source)
    guard !id.isEmpty else {
      throw DecodingError.dataCorruptedError(
        forKey: .id, in: container, debugDescription: "Expected a nonempty opaque candidate ID")
    }
    // The key is required even when selection is unknown.
    isSelected = try container.decode(Bool?.self, forKey: .isSelected)
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(id, forKey: .id)
    try container.encode(source, forKey: .source)
    try container.encode(isSelected, forKey: .isSelected)
  }
}

/// A bounded, sorted observation of retained files, weakly consistent across separate calls.
public struct ConfigurationSnapshotCandidatePage: Codable, Equatable, Sendable {
  /// At most 256 references in strictly increasing ID order.
  public let candidates: [ConfigurationSnapshotCandidateReference]
  /// The last returned ID when another page may exist; pass it unchanged as `after`.
  public let nextCursor: String?

  private enum CodingKeys: String, CodingKey {
    case candidates, nextCursor
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    candidates = try container.decode(
      [ConfigurationSnapshotCandidateReference].self, forKey: .candidates)
    guard candidates.count <= 256,
      zip(candidates, candidates.dropFirst()).allSatisfy({ $0.0.id < $0.1.id })
    else {
      throw DecodingError.dataCorruptedError(
        forKey: .candidates, in: container,
        debugDescription: "Expected at most 256 sorted, unique candidate references")
    }
    nextCursor = try container.decode(String?.self, forKey: .nextCursor)
    if let nextCursor, nextCursor != candidates.last?.id {
      throw DecodingError.dataCorruptedError(
        forKey: .nextCursor, in: container,
        debugDescription: "Expected the last returned candidate ID or null")
    }
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(candidates, forKey: .candidates)
    try container.encode(nextCursor, forKey: .nextCursor)
  }
}

/// Verified immutable bytes read from a private copy, before host semantic validation.
public struct ConfigurationSnapshotCandidate: Codable, Equatable, Sendable {
  /// An opaque managed file identity; never interpret it as a path or write generation.
  public let id: String
  public let source: ConfigurationSnapshotCandidateSource
  public let snapshot: ConfigurationSnapshot
  /// File modification time in signed Unix seconds, with the engine's fractional precision.
  public let modifiedAtUnixSeconds: Double
  /// The time as a Foundation date, suitable for presentation.
  public var modifiedAt: Date { Date(timeIntervalSince1970: modifiedAtUnixSeconds) }
  /// `nil` means selection cannot be determined for this source.
  public let isSelected: Bool?

  private enum CodingKeys: String, CodingKey {
    case id, source, snapshot, modifiedAt, isSelected
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    id = try container.decode(String.self, forKey: .id)
    source = try container.decode(ConfigurationSnapshotCandidateSource.self, forKey: .source)
    guard !id.isEmpty else {
      throw DecodingError.dataCorruptedError(
        forKey: .id, in: container, debugDescription: "Expected a nonempty opaque candidate ID")
    }
    snapshot = try container.decode(ConfigurationSnapshot.self, forKey: .snapshot)
    guard snapshot.generation != nil else {
      throw DecodingError.dataCorruptedError(
        forKey: .snapshot, in: container,
        debugDescription: "Expected an initialized candidate snapshot")
    }
    modifiedAtUnixSeconds = try container.decodeFiniteUnixSeconds(forKey: .modifiedAt)
    isSelected = try container.decode(Bool?.self, forKey: .isSelected)
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(id, forKey: .id)
    try container.encode(source, forKey: .source)
    try container.encode(snapshot, forKey: .snapshot)
    try container.encode(modifiedAtUnixSeconds, forKey: .modifiedAt)
    try container.encode(isSelected, forKey: .isSelected)
  }
}
