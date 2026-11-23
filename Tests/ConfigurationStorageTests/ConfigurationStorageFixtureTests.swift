import ConfigurationStorage
import ConfigurationStorageTestSupport
import Foundation
import XCTest

final class ConfigurationStorageFixtureTests: XCTestCase {
  func testEveryMalformedStateIsRejectedWithoutReplacingItsOriginalBytes() async throws {
    for corruption in ConfigurationStorageFixture.Corruption.allCases {
      let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
      defer { try? FileManager.default.removeItem(at: root) }
      let fixture = ConfigurationStorageFixture(directory: root, owner: "test.fixture")
      let store = ConfigurationStore(configuration: .init(directory: root, owner: fixture.owner))
      _ = try await store.save(expectedGeneration: nil, payload: Data([1, 2, 3]))
      try fixture.corrupt(corruption)
      let selected = try fixture.databaseURL()
      let bytes = try Data(contentsOf: selected)
      do {
        _ = try await store.load()
        XCTFail("Expected malformed state rejection")
      } catch let error as ConfigurationStorageError {
        if corruption == .numericPayload || corruption == .textPayload {
          XCTAssertTrue([.ioFailure, .invalidStoredData].contains(error.code), "\(corruption)")
        } else {
          XCTAssertEqual(
            error.code, corruption == .foreignOwner ? .ownerMismatch : .invalidStoredData,
            "\(corruption)")
        }
        if corruption == .foreignOwner {
          XCTAssertNil(error.recoveryDirectory)
          XCTAssertEqual(try Data(contentsOf: selected), bytes)
          continue
        }
        let recovery = try XCTUnwrap(error.recoveryDirectory)
        XCTAssertEqual(
          try Data(contentsOf: recovery.appendingPathComponent(selected.lastPathComponent)), bytes)
      }
      XCTAssertEqual(try Data(contentsOf: selected), bytes)
    }
  }
}
