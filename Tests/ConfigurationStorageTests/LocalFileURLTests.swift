import ConfigurationStorage
import Foundation
import XCTest

@MainActor
final class LocalFileURLTests: XCTestCase {
  func testNonLocalStoreAuthorityCannotInitializeTheSameLocalPath() async throws {
    let root = try temporaryRoot()
    defer { try? FileManager.default.removeItem(at: root) }
    let local = root.appendingPathComponent("store")
    let remote = try withHost("storage.example.invalid", local)
    let store = ConfigurationStore(
      configuration: .init(directory: remote, owner: "com.example.url-fixture"))

    do {
      _ = try await store.save(expectedGeneration: nil, payload: Data("unaccepted".utf8))
      XCTFail("A non-local authority must not be discarded before opening a store.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidConfiguration)
    }
    XCTAssertFalse(FileManager.default.fileExists(atPath: local.path))
  }

  func testNonLocalRecoveryAuthorityCannotReadOrReplaceTheSameLocalFile() async throws {
    let root = try temporaryRoot()
    defer { try? FileManager.default.removeItem(at: root) }
    let local = root.appendingPathComponent("reviewed.copy")
    try await ConfigurationRecoveryCopy.write(payload: Data("reviewed".utf8), to: local)
    let original = try Data(contentsOf: local)
    let remote = try withHost("storage.example.invalid", local)

    do {
      _ = try await ConfigurationRecoveryCopy.read(from: remote)
      XCTFail("A remote recovery URL must not read a local file with the same path.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidConfiguration)
    }
    do {
      try await ConfigurationRecoveryCopy.write(payload: Data("unaccepted".utf8), to: remote)
      XCTFail("A remote recovery URL must not replace a local file with the same path.")
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidConfiguration)
    }
    XCTAssertEqual(try Data(contentsOf: local), original)
  }

  func testLocalAndLocalhostAuthoritiesRetainStoreAndRecoveryCompatibility() async throws {
    let root = try temporaryRoot()
    defer { try? FileManager.default.removeItem(at: root) }
    for host in [nil, "", "localhost", "LOCALHOST"] as [String?] {
      let local = root.appendingPathComponent(UUID().uuidString)
      let selected = try withHost(host, local)
      let store = ConfigurationStore(
        configuration: .init(directory: selected, owner: "com.example.url-fixture"))
      let saved = try await store.save(expectedGeneration: nil, payload: Data("accepted".utf8))
      let reopened = ConfigurationStore(
        configuration: .init(directory: local, owner: "com.example.url-fixture"))
      let loaded = try await reopened.load()
      XCTAssertEqual(loaded, saved)

      let copy = root.appendingPathComponent(UUID().uuidString + ".copy")
      try await ConfigurationRecoveryCopy.write(
        payload: XCTUnwrap(saved.payload), to: withHost(host, copy))
      let read = try await ConfigurationRecoveryCopy.read(from: withHost(host, copy))
      XCTAssertEqual(read.payload, saved.payload)
    }
  }

  func testUnsupportedLocalhostAuthorityCannotInitializeTheSameLocalPath() async throws {
    let root = try temporaryRoot()
    defer { try? FileManager.default.removeItem(at: root) }
    for userInfo in [false, true] {
      let local = root.appendingPathComponent(UUID().uuidString)
      var components = try XCTUnwrap(URLComponents(url: local, resolvingAgainstBaseURL: false))
      components.host = "localhost"
      if userInfo { components.user = "fixture-user" } else { components.port = 12345 }
      let store = ConfigurationStore(
        configuration: .init(
          directory: try XCTUnwrap(components.url), owner: "com.example.url-fixture"))
      do {
        _ = try await store.save(expectedGeneration: nil, payload: Data("unaccepted".utf8))
        XCTFail("Native paths cannot represent a file URL's user information or port.")
      } catch let error as ConfigurationStorageError {
        XCTAssertEqual(error.code, .invalidConfiguration)
      }
      XCTAssertFalse(FileManager.default.fileExists(atPath: local.path))
    }
  }

  func testRecoveryWriteRejectsAmbiguousURLsWithoutReplacingExistingFiles() async throws {
    let root = try temporaryRoot()
    defer { try? FileManager.default.removeItem(at: root) }
    for (name, encodedSpelling) in [
      ("reviewed.copy", "reviewed.copy/"),
      ("%00.copy", "%00.copy"),
      ("%2F.copy", "%2F.copy"),
    ] {
      let local = root.appendingPathComponent(name, isDirectory: false)
      let payload = Data("reviewed".utf8)
      try await ConfigurationRecoveryCopy.write(payload: payload, to: local)
      let selected = try XCTUnwrap(URL(string: root.absoluteString + "/" + encodedSpelling))

      do {
        try await ConfigurationRecoveryCopy.write(payload: Data("unaccepted".utf8), to: selected)
        XCTFail("The alternate URL spelling must not replace the valid local file.")
      } catch let error as ConfigurationStorageError {
        XCTAssertEqual(error.code, .invalidConfiguration)
      }
      let copy = try await ConfigurationRecoveryCopy.read(from: local)
      XCTAssertEqual(copy.payload, payload)
    }
  }

  func testQueryAndFragmentCannotReadReplaceOrClaimTheBarePath() async throws {
    let root = try temporaryRoot()
    defer { try? FileManager.default.removeItem(at: root) }
    let local = root.appendingPathComponent("reviewed.copy")
    try await ConfigurationRecoveryCopy.write(payload: Data("reviewed".utf8), to: local)
    let original = try Data(contentsOf: local)

    for suffix in ["?revision=old", "?", "?%FF", "#review", "#", "#%FF"] {
      let selected = try XCTUnwrap(URL(string: local.absoluteString + suffix))
      await assertInvalidConfiguration {
        _ = try await ConfigurationRecoveryCopy.read(from: selected)
      }
      await assertInvalidConfiguration {
        try await ConfigurationRecoveryCopy.write(payload: Data("changed".utf8), to: selected)
      }
      XCTAssertEqual(try Data(contentsOf: local), original, suffix)

      let wouldBeStore = root.appendingPathComponent(UUID().uuidString, isDirectory: true)
      let storeURL = try XCTUnwrap(URL(string: wouldBeStore.absoluteString + suffix))
      await assertInvalidConfiguration {
        _ = try await ConfigurationStore.open(
          configuration: .init(directory: storeURL, owner: "com.example.url-fixture"))
      }
      XCTAssertFalse(FileManager.default.fileExists(atPath: wouldBeStore.path), suffix)
    }
  }

  func testEscapedQuestionMarkAndHashRemainPartOfStoreAndRecoveryPaths() async throws {
    let root = try temporaryRoot()
    defer { try? FileManager.default.removeItem(at: root) }
    let configuration = ConfigurationStore.Configuration(
      directory: root.appendingPathComponent("store?#review", isDirectory: true),
      owner: "com.example.url-fixture")
    let store = try await ConfigurationStore.open(configuration: configuration)
    let payload = Data("reviewed".utf8)
    let saved = try await store.save(expectedGeneration: nil, payload: payload)
    let reopened = try await ConfigurationStore.openExisting(configuration: configuration)
    let loaded = try await reopened.load()
    XCTAssertEqual(loaded, saved)
    XCTAssertFalse(
      FileManager.default.fileExists(atPath: root.appendingPathComponent("store").path))

    let copy = root.appendingPathComponent("snapshot.copy?#review")
    try await ConfigurationRecoveryCopy.write(payload: payload, to: copy)
    let read = try await ConfigurationRecoveryCopy.read(from: copy)
    XCTAssertEqual(read.payload, payload)
    XCTAssertTrue(FileManager.default.fileExists(atPath: copy.path))
    XCTAssertFalse(
      FileManager.default.fileExists(atPath: root.appendingPathComponent("snapshot.copy").path))
  }

  private func assertInvalidConfiguration(
    _ operation: () async throws -> Void, file: StaticString = #filePath, line: UInt = #line
  ) async {
    do {
      try await operation()
      XCTFail("Expected an invalid configuration error.", file: file, line: line)
    } catch let error as ConfigurationStorageError {
      XCTAssertEqual(error.code, .invalidConfiguration, file: file, line: line)
    } catch {
      XCTFail("Unexpected error: \(error)", file: file, line: line)
    }
  }

  private func temporaryRoot() throws -> URL {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    return root
  }

  private func withHost(_ host: String?, _ url: URL) throws -> URL {
    var components = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false))
    components.host = host
    return try XCTUnwrap(components.url)
  }
}
