import CryptoKit
import Foundation
import SQLite3

/// Explicit destructive fixtures for exclusively owned temporary stores. Never use an installed store.
public struct ConfigurationStorageFixture {
  public enum Corruption: CaseIterable, Sendable {
    case numericPayload, textPayload, emptyGeneration, emptyDigest, invalidDigest, missingState,
      foreignOwner
  }

  public let directory: URL
  public let owner: String

  /// Selects a test directory only; construction performs no I/O or automatic cleanup.
  public init(directory: URL, owner: String) {
    self.directory = directory
    self.owner = owner
  }

  public var selectionURL: URL { directory.appendingPathComponent("active-generation") }
  public var backupURL: URL {
    get throws {
      let marker = directory.appendingPathComponent("active-backup")
      guard FileManager.default.fileExists(atPath: marker.path) else {
        return directory.appendingPathComponent("Backups/snapshot.sqlite")
      }
      let name = try String(contentsOf: marker, encoding: .utf8)
      guard let id = UUID(uuidString: name), id.uuidString.lowercased() == name else {
        throw CocoaError(.fileReadCorruptFile)
      }
      return directory.appendingPathComponent("Backups").appendingPathComponent(name + ".sqlite")
    }
  }

  public func databaseURL() throws -> URL {
    let name = try String(contentsOf: selectionURL, encoding: .utf8)
    guard let id = UUID(uuidString: name), id.uuidString.lowercased() == name else {
      throw CocoaError(.fileReadCorruptFile)
    }
    return directory.appendingPathComponent("Generations").appendingPathComponent(name + ".sqlite")
  }

  public func generation() throws -> Data? {
    guard FileManager.default.fileExists(atPath: selectionURL.path) else { return nil }
    return try FixtureDatabase(databaseURL()).text("SELECT generation FROM store_state WHERE id=1")
      .map { Data($0.utf8) }
  }

  public func payload(at url: URL? = nil) throws -> Data {
    guard
      let data = try FixtureDatabase(url ?? databaseURL()).blob(
        "SELECT payload FROM store_state WHERE id=1")
    else { throw CocoaError(.fileReadCorruptFile) }
    return data
  }

  public func replacePayload(_ data: Data, at url: URL? = nil) throws {
    try FixtureDatabase(url ?? databaseURL()).execute(
      "UPDATE store_state SET payload=?, digest=? WHERE id=1", values: [data, Self.digest(data)])
  }

  public func payloadDigestMatches(at url: URL? = nil) throws -> Bool {
    let selected = try url ?? databaseURL()
    return try FixtureDatabase(selected).text("SELECT digest FROM store_state WHERE id=1")
      == Self.digest(payload(at: selected))
  }

  public func rejectWrites(named name: String) throws {
    try validateIdentifier(name)
    try FixtureDatabase(databaseURL()).execute(
      "CREATE TRIGGER \(name) AFTER UPDATE ON store_state BEGIN SELECT RAISE(ABORT, 'test fixture'); END"
    )
  }

  public func allowWrites(named name: String) throws {
    try validateIdentifier(name)
    try FixtureDatabase(databaseURL()).execute("DROP TRIGGER \(name)")
  }

  public func setStorageVersion(_ version: Int, at url: URL? = nil) throws {
    guard version >= 0 else { throw FixtureInputError.invalidValue }
    try FixtureDatabase(url ?? databaseURL()).execute("PRAGMA user_version=\(version)")
  }

  public func corrupt(_ corruption: Corruption, at url: URL? = nil) throws {
    let sql: String
    switch corruption {
    case .numericPayload: sql = "UPDATE store_state SET payload=42 WHERE id=1"
    case .textPayload: sql = "UPDATE store_state SET payload='not a blob' WHERE id=1"
    case .emptyGeneration: sql = "UPDATE store_state SET generation='' WHERE id=1"
    case .emptyDigest: sql = "UPDATE store_state SET digest='' WHERE id=1"
    case .invalidDigest: sql = "UPDATE store_state SET digest='A'||substr(digest,2) WHERE id=1"
    case .missingState: sql = "DELETE FROM store_state WHERE id=1"
    case .foreignOwner: sql = "UPDATE store_state SET owner='other.owner' WHERE id=1"
    }
    try FixtureDatabase(url ?? databaseURL()).execute(sql)
  }

  public func obstructGenerationCreation() throws {
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    try Data("test initialization obstruction".utf8).write(
      to: directory.appendingPathComponent("Generations"), options: .withoutOverwriting)
  }

  public func removeGenerationObstruction() throws {
    let url = directory.appendingPathComponent("Generations")
    guard try Data(contentsOf: url) == Data("test initialization obstruction".utf8) else {
      throw FixtureInputError.invalidValue
    }
    try FileManager.default.removeItem(at: url)
  }

  public func installUnreadable(_ data: Data, journal: Data? = nil) throws -> URL {
    let id = UUID().uuidString.lowercased()
    let generations = directory.appendingPathComponent("Generations")
    try FileManager.default.createDirectory(at: generations, withIntermediateDirectories: true)
    try Data(owner.utf8).write(to: directory.appendingPathComponent("store-owner"))
    try Data(id.utf8).write(to: selectionURL)
    let selected = generations.appendingPathComponent(id + ".sqlite")
    try data.write(to: selected)
    try journal?.write(to: URL(fileURLWithPath: selected.path + "-journal"))
    return selected
  }

  private func validateIdentifier(_ value: String) throws {
    guard !value.isEmpty,
      value.utf8.allSatisfy({
        (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 95
      })
    else { throw FixtureInputError.invalidValue }
  }

  private static func digest(_ data: Data) -> String {
    SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
  }
}

private final class FixtureDatabase {
  private var handle: OpaquePointer?
  init(_ url: URL) throws {
    guard FileManager.default.fileExists(atPath: url.path),
      sqlite3_open_v2(url.path, &handle, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK
    else {
      sqlite3_close(handle)
      throw CocoaError(.fileReadCorruptFile)
    }
  }
  deinit { sqlite3_close(handle) }

  func execute(_ sql: String, values: [Any] = []) throws {
    if values.isEmpty {
      guard sqlite3_exec(handle, sql, nil, nil, nil) == SQLITE_OK else {
        throw CocoaError(.fileWriteUnknown)
      }
      return
    }
    let statement = try prepare(sql)
    defer { sqlite3_finalize(statement) }
    let copy = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
    for (index, value) in values.enumerated() {
      let result: Int32
      if let data = value as? Data {
        guard data.count <= Int32.max else { throw FixtureInputError.invalidValue }
        result =
          data.isEmpty
          ? sqlite3_bind_zeroblob(statement, Int32(index + 1), 0)
          : data.withUnsafeBytes {
            sqlite3_bind_blob(statement, Int32(index + 1), $0.baseAddress, Int32($0.count), copy)
          }
      } else if let text = value as? String {
        result = sqlite3_bind_text(statement, Int32(index + 1), text, -1, copy)
      } else {
        throw FixtureInputError.invalidValue
      }
      guard result == SQLITE_OK else { throw CocoaError(.fileWriteUnknown) }
    }
    guard sqlite3_step(statement) == SQLITE_DONE else { throw CocoaError(.fileWriteUnknown) }
  }

  func text(_ sql: String) throws -> String? {
    let statement = try prepare(sql)
    defer { sqlite3_finalize(statement) }
    guard sqlite3_step(statement) == SQLITE_ROW else { throw CocoaError(.fileReadCorruptFile) }
    guard let bytes = sqlite3_column_text(statement, 0) else { return nil }
    return String(cString: bytes)
  }

  func blob(_ sql: String) throws -> Data? {
    let statement = try prepare(sql)
    defer { sqlite3_finalize(statement) }
    guard sqlite3_step(statement) == SQLITE_ROW else { throw CocoaError(.fileReadCorruptFile) }
    let count = Int(sqlite3_column_bytes(statement, 0))
    guard let bytes = sqlite3_column_blob(statement, 0) else { return count == 0 ? Data() : nil }
    return Data(bytes: bytes, count: count)
  }

  private func prepare(_ sql: String) throws -> OpaquePointer {
    var statement: OpaquePointer?
    guard sqlite3_prepare_v2(handle, sql, -1, &statement, nil) == SQLITE_OK, let statement else {
      throw CocoaError(.fileReadCorruptFile)
    }
    return statement
  }
}

private enum FixtureInputError: Error { case invalidValue }
