import CConfigurationStorage
import Foundation

// Preserve directory spelling and reject URL components the native path cannot represent.
enum NativeFilePath {
  static func local(_ url: URL) throws -> String {
    let path = url.path(percentEncoded: false)
    guard url.isFileURL,
      url.host.map({ $0.isEmpty || $0.lowercased() == "localhost" }) ?? true,
      url.user == nil, url.password == nil, url.port == nil,
      url.query(percentEncoded: true) == nil, url.fragment(percentEncoded: true) == nil,
      path.hasPrefix("/"), !path.contains("\0"),
      url.path(percentEncoded: true).range(of: "%2f", options: .caseInsensitive) == nil
    else { throw ConfigurationStorageError(.invalidConfiguration) }
    return path
  }
}

// Require the source contract even if an old framework ships matching old headers.
enum NativeABI {
  static let supportedVersion: UInt32 = 14

  static func requireCompatible(
    headerVersion: UInt32 = CONFIGURATION_STORAGE_ABI_VERSION,
    runtimeVersion: () -> UInt32 = { configuration_storage_abi_version() }
  ) throws {
    guard headerVersion == supportedVersion, runtimeVersion() == supportedVersion else {
      throw ConfigurationStorageError(.unsupportedABIVersion)
    }
  }
}

// The actor exclusively owns this handle. ARC destroys it after its last synchronous dispatch.
final class NativeStore {
  enum Admission {
    case createOrOpen, existingOnly
  }

  private let handle: OpaquePointer
  private let maximumPayloadBytes: Int

  init(
    configuration: ConfigurationStore.Configuration, admission: Admission = .createOrOpen
  ) throws {
    try NativeABI.requireCompatible()
    let directory = try NativeFilePath.local(configuration.directory)
    let input = try NativeJSON.encode(
      OpenConfiguration(
        directory: directory,
        owner: configuration.owner,
        maximumPayloadBytes: configuration.maximumPayloadBytes))
    let open =
      switch admission {
      case .createOrOpen: configuration_storage_store_open
      case .existingOnly: configuration_storage_store_open_existing
      }
    var failure: OpaquePointer?
    try Task.checkCancellation()
    let opened = input.withUnsafeBytes { bytes in
      open(bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count, &failure)
    }
    if let failure {
      if let opened { configuration_storage_store_destroy(opened) }
      _ = try NativeJSON.consume(failure)
      throw ConfigurationStorageError(.invalidNativeResponse)
    }
    guard let opened else { throw ConfigurationStorageError(.invalidNativeResponse) }
    handle = opened
    maximumPayloadBytes = configuration.maximumPayloadBytes
  }

  deinit {
    configuration_storage_store_destroy(handle)
  }

  func dispatch<Value: Decodable>(_ command: StorageCommand) throws -> Value {
    try NativeJSON.dispatch(
      command, payload: command.payload, maximumPayloadBytes: maximumPayloadBytes
    ) { metadata, length, payload, payloadLength in
      configuration_storage_store_request(handle, metadata, length, payload, payloadLength)
    }
  }

  private struct OpenConfiguration: Encodable {
    let directory: String
    let owner: String
    let maximumPayloadBytes: Int
  }
}

protocol NativePayloadResponse {}

extension ConfigurationSnapshot: NativePayloadResponse {}
extension ConfigurationBackup: NativePayloadResponse {}
extension ConfigurationBackupPreparation: NativePayloadResponse {}
extension ConfigurationSnapshotCandidate: NativePayloadResponse {}
extension ConfigurationRecoveryCopy: NativePayloadResponse {}

/// Both store commands and independent file commands separate metadata from opaque bytes.
enum NativeJSON {
  private static let payloadKey = CodingUserInfoKey(rawValue: "ConfigurationStorage.nativePayload")!

  private struct FramedPayload: Sendable {
    let bytes: Data?
  }

  struct Reply: Sendable {
    let metadata: Data
    let payload: Data?

    func decode<Value: Decodable>(_ type: Value.Type) throws -> Value {
      guard payload == nil || type is any NativePayloadResponse.Type else {
        throw ConfigurationStorageError(.invalidNativeResponse)
      }
      let decoder = JSONDecoder()
      decoder.userInfo[payloadKey] = FramedPayload(bytes: payload)
      do {
        return try decoder.decode(type, from: metadata)
      } catch {
        throw ConfigurationStorageError(.invalidNativeResponse)
      }
    }
  }

  static func hasPayload(_ decoder: any Decoder) -> Bool {
    (decoder.userInfo[payloadKey] as? FramedPayload)?.bytes != nil
  }

  static func decodePayload<Key: CodingKey>(
    from container: KeyedDecodingContainer<Key>, key: Key, decoder: any Decoder
  ) throws -> Data? {
    if let framed = decoder.userInfo[payloadKey] as? FramedPayload {
      guard !container.contains(key) else {
        throw DecodingError.dataCorruptedError(
          forKey: key, in: container, debugDescription: "Expected separately framed payload")
      }
      return framed.bytes
    }
    let bytes = try container.decode(Data?.self, forKey: key)
    if let bytes, bytes.count > ConfigurationStore.Configuration.maximumSupportedPayloadBytes {
      throw DecodingError.dataCorruptedError(
        forKey: key, in: container, debugDescription: "Payload exceeds the supported byte limit")
    }
    return bytes
  }

  static func encode(_ command: some Encodable) throws -> Data { try JSONEncoder().encode(command) }

  static func dispatch<Value: Decodable>(
    _ command: some Encodable,
    payload: Data? = nil,
    maximumPayloadBytes: Int = ConfigurationStore.Configuration.maximumSupportedPayloadBytes,
    using nativeEntry: (UnsafePointer<UInt8>?, Int, UnsafePointer<UInt8>?, Int) -> OpaquePointer?
  ) throws -> Value {
    try NativeABI.requireCompatible()
    let input = try encode(command)
    try Task.checkCancellation()
    // No cancellation check after entry: a confirmed file write stays confirmed.
    let owner = input.withUnsafeBytes { metadata in
      let command = metadata.bindMemory(to: UInt8.self)
      guard let payload, !payload.isEmpty else {
        return nativeEntry(command.baseAddress, command.count, nil, 0)
      }
      return payload.withUnsafeBytes { bytes in
        let payload = bytes.bindMemory(to: UInt8.self)
        return nativeEntry(command.baseAddress, command.count, payload.baseAddress, payload.count)
      }
    }
    return try consume(owner, maximumPayloadBytes: maximumPayloadBytes).decode(Value.self)
  }

  static func consume(
    _ owner: OpaquePointer?,
    maximumPayloadBytes: Int = ConfigurationStore.Configuration.maximumSupportedPayloadBytes
  ) throws -> Reply {
    guard let owner else { throw ConfigurationStorageError(.invalidNativeResponse) }
    defer { configuration_storage_result_destroy(owner) }
    let view = configuration_storage_result_view_v14(owner)
    guard let bytes = view.data, view.length > 0, view.length <= 1024 * 1024,
      view.payload_length >= 0, view.payload_length <= maximumPayloadBytes,
      view.payload != nil || view.payload_length == 0,
      view.code == 0 || (view.code == 1 && view.payload == nil)
    else {
      throw ConfigurationStorageError(.invalidNativeResponse)
    }
    let metadata = Data(bytes: bytes, count: view.length)
    guard view.code == 0 else {
      throw (try? JSONDecoder().decode(ConfigurationStorageError.self, from: metadata))
        ?? ConfigurationStorageError(.invalidNativeResponse)
    }
    guard let payload = view.payload else { return Reply(metadata: metadata, payload: nil) }
    guard view.payload_length > 0 else { return Reply(metadata: metadata, payload: Data()) }
    return Reply(metadata: metadata, payload: Data(bytes: payload, count: view.payload_length))
  }

  struct Null: Decodable {
    init(from decoder: any Decoder) throws {
      guard try decoder.singleValueContainer().decodeNil() else {
        throw ConfigurationStorageError(.invalidNativeResponse)
      }
    }
  }
}

enum StorageCommand: Encodable {
  case load
  case listSnapshotCandidates(
    limit: Int, after: String?, source: ConfigurationSnapshotCandidateSource?)
  case readSnapshotCandidate(id: String)
  case save(expectedGeneration: String?, payload: Data)
  case prepareBackup(expectedGeneration: String)
  case createBackup(expectedGeneration: String, token: String)
  case prepareBackupRebuild(expectedGeneration: String)
  case rebuildBackup(expectedGeneration: String, token: String)
  case readBackup
  case restoreBackup(token: String)
  case prepareRestore(payload: Data)
  case restoreSnapshot(payload: Data, token: String)

  private enum CodingKeys: String, CodingKey {
    case command, expectedGeneration, token, limit, after, id, source
  }

  var payload: Data? {
    switch self {
    case .save(_, let payload), .prepareRestore(let payload), .restoreSnapshot(let payload, _):
      payload
    default:
      nil
    }
  }

  func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    switch self {
    case .load:
      try container.encode("load", forKey: .command)
    case .listSnapshotCandidates(let limit, let after, let source):
      try container.encode("listSnapshotCandidates", forKey: .command)
      try container.encode(limit, forKey: .limit)
      try container.encode(after, forKey: .after)
      try container.encode(source, forKey: .source)
    case .readSnapshotCandidate(let id):
      try container.encode("readSnapshotCandidate", forKey: .command)
      try container.encode(id, forKey: .id)
    case .save(let expectedGeneration, _):
      try container.encode("save", forKey: .command)
      try container.encode(expectedGeneration, forKey: .expectedGeneration)
    case .prepareBackup(let expectedGeneration):
      try container.encode("prepareBackup", forKey: .command)
      try container.encode(expectedGeneration, forKey: .expectedGeneration)
    case .createBackup(let expectedGeneration, let token):
      try container.encode("createBackup", forKey: .command)
      try container.encode(expectedGeneration, forKey: .expectedGeneration)
      try container.encode(token, forKey: .token)
    case .prepareBackupRebuild(let expectedGeneration):
      try container.encode("prepareBackupRebuild", forKey: .command)
      try container.encode(expectedGeneration, forKey: .expectedGeneration)
    case .rebuildBackup(let expectedGeneration, let token):
      try container.encode("rebuildBackup", forKey: .command)
      try container.encode(expectedGeneration, forKey: .expectedGeneration)
      try container.encode(token, forKey: .token)
    case .readBackup:
      try container.encode("readBackup", forKey: .command)
    case .restoreBackup(let token):
      try container.encode("restoreBackup", forKey: .command)
      try container.encode(token, forKey: .token)
    case .prepareRestore:
      try container.encode("prepareRestore", forKey: .command)
    case .restoreSnapshot(_, let token):
      try container.encode("restoreSnapshot", forKey: .command)
      try container.encode(token, forKey: .token)
    }
  }
}
