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
  static let supportedVersion: UInt32 = 13

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
    try NativeJSON.dispatch(command, maximumPayloadBytes: maximumPayloadBytes) { bytes, length in
      configuration_storage_store_dispatch(handle, bytes, length)
    }
  }

  private struct OpenConfiguration: Encodable {
    let directory: String
    let owner: String
    let maximumPayloadBytes: Int
  }
}

/// Both store commands and independent file commands transfer result ownership to response Data.
enum NativeJSON {
  private static let payloadLimitKey = CodingUserInfoKey(
    rawValue: "ConfigurationStorage.maximumPayloadBytes")!

  static func payloadLimit(from decoder: any Decoder) -> Int {
    decoder.userInfo[payloadLimitKey] as? Int
      ?? ConfigurationStore.Configuration.maximumSupportedPayloadBytes
  }

  static func encode(_ command: some Encodable) throws -> Data {
    let encoder = JSONEncoder()
    encoder.dataEncodingStrategy = .base64
    // This JSON goes directly to the ABI. Escaping base64 slashes can exceed its
    // request limit even when the opaque payload is within the configured limit.
    encoder.outputFormatting = .withoutEscapingSlashes
    return try encoder.encode(command)
  }

  static func dispatch<Value: Decodable>(
    _ command: some Encodable,
    maximumPayloadBytes: Int = ConfigurationStore.Configuration.maximumSupportedPayloadBytes,
    using nativeEntry: (UnsafePointer<UInt8>?, Int) -> OpaquePointer?
  ) throws -> Value {
    try NativeABI.requireCompatible()
    let input = try encode(command)
    try Task.checkCancellation()
    // No cancellation check after entry: a confirmed file write stays confirmed.
    let output = try input.withUnsafeBytes { bytes in
      try consume(nativeEntry(bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count))
    }
    do {
      let decoder = JSONDecoder()
      decoder.dataDecodingStrategy = .base64
      decoder.userInfo[payloadLimitKey] = maximumPayloadBytes
      return try decoder.decode(Value.self, from: output)
    } catch {
      throw ConfigurationStorageError(.invalidNativeResponse)
    }
  }

  static func consume(_ owner: OpaquePointer?) throws -> Data {
    guard let owner else { throw ConfigurationStorageError(.invalidNativeResponse) }
    let view = configuration_storage_result_view(owner)
    guard let bytes = view.data, view.length > 0, view.length <= 96 * 1024 * 1024,
      view.code == 0 || view.code == 1
    else {
      configuration_storage_result_destroy(owner)
      throw ConfigurationStorageError(.invalidNativeResponse)
    }
    // Data takes ownership and releases the result when its storage no longer needs it.
    let data = Data(
      bytesNoCopy: UnsafeMutableRawPointer(mutating: bytes), count: view.length,
      deallocator: .custom { _, _ in configuration_storage_result_destroy(owner) })
    guard view.code == 0 else {
      throw (try? JSONDecoder().decode(ConfigurationStorageError.self, from: data))
        ?? ConfigurationStorageError(.invalidNativeResponse)
    }
    return data
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
    case command, expectedGeneration, payload, token, limit, after, id, source
  }

  func exceedsPayloadLimit(_ maximum: Int) -> Bool {
    switch self {
    case .save(_, let payload), .prepareRestore(let payload), .restoreSnapshot(let payload, _):
      payload.count > maximum
    default:
      false
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
    case .save(let expectedGeneration, let payload):
      try container.encode("save", forKey: .command)
      try container.encode(expectedGeneration, forKey: .expectedGeneration)
      try container.encode(payload, forKey: .payload)
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
    case .prepareRestore(let payload):
      try container.encode("prepareRestore", forKey: .command)
      try container.encode(payload, forKey: .payload)
    case .restoreSnapshot(let payload, let token):
      try container.encode("restoreSnapshot", forKey: .command)
      try container.encode(payload, forKey: .payload)
      try container.encode(token, forKey: .token)
    }
  }
}
