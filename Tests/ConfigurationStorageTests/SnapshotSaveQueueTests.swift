import ConfigurationStorage
import Foundation
import XCTest

@MainActor
final class SnapshotSaveQueueTests: XCTestCase {
  func testEnqueueNeverWritesAndExplicitFlushCoalescesTheLatestSnapshot() async throws {
    let sink = ControlledConfigurationSink()
    let saves = try SnapshotSaveQueue<Int> { try await sink.save($0) }
    try await saves.flush()
    try saves.enqueue(1)
    try saves.enqueue(2)
    let beforeFlush = await sink.values
    XCTAssertTrue(beforeFlush.isEmpty)
    let flush = Task { try await saves.flush() }
    await sink.waitForAttempts(1)
    let values = await sink.values
    XCTAssertEqual(values, [2])
    await sink.finishNext()
    try await flush.value
    try await saves.flush()
    XCTAssertFalse(saves.state.hasUnsavedChanges)
    XCTAssertEqual(saves.state.committedRevision, 2)
  }

  func testSlowWriteAllowsUIEditsAndFlushDrainsLatestRevisionInOrder() async throws {
    let sink = ControlledConfigurationSink()
    let saves = try SnapshotSaveQueue<Int> { try await sink.save($0) }
    try saves.enqueue(1)
    let flush = Task { try await saves.flush() }
    await sink.waitForAttempts(1)
    try saves.enqueue(2)
    try saves.enqueue(3)
    await sink.finishNext()
    await sink.waitForAttempts(2)
    try saves.enqueue(4)
    await sink.finishNext()
    await sink.waitForAttempts(3)
    await sink.finishNext()
    try await flush.value
    let values = await sink.values
    let maximumActive = await sink.maximumActive
    XCTAssertEqual(values, [1, 3, 4])
    XCTAssertEqual(maximumActive, 1)
    XCTAssertEqual(saves.state.committedRevision, 4)
    XCTAssertFalse(saves.state.hasUnsavedChanges)
  }

  func testOlderFailedWriteDoesNotRestoreObsoleteEdits() async throws {
    let sink = ControlledConfigurationSink()
    let saves = try SnapshotSaveQueue<Int> { try await sink.save($0) }
    try saves.enqueue(1)
    let flush = Task { try await saves.flush() }
    await sink.waitForAttempts(1)
    try saves.enqueue(2)
    await sink.finishNext(fail: true)
    await sink.waitForAttempts(2)
    await sink.finishNext()
    try await flush.value
    let values = await sink.values
    XCTAssertEqual(values, [1, 2])
    XCTAssertEqual(saves.state.committedRevision, 2)
    XCTAssertFalse(saves.state.hasUnsavedChanges)
  }

  func testConcurrentFlushSharesFailureAndExplicitRetryRetainsTheSameSnapshot() async throws {
    let sink = ControlledConfigurationSink()
    let saves = try SnapshotSaveQueue<Int> { try await sink.save($0) }
    try saves.enqueue(7)
    let first = Task { try await saves.flush() }
    await sink.waitForAttempts(1)
    var joined = false
    let second = Task {
      joined = true
      try await saves.flush()
    }
    while !joined { await Task.yield() }
    await sink.finishNext(fail: true)
    for task in [first, second] {
      do {
        try await task.value
        XCTFail("Expected held write failure")
      } catch { XCTAssertEqual(error as? ControlledConfigurationSink.Failure, .expected) }
    }
    let beforeRetry = await sink.values
    XCTAssertEqual(beforeRetry, [7])
    XCTAssertTrue(saves.state.hasUnsavedChanges)
    XCTAssertEqual(saves.state.committedRevision, 0)
    let retry = Task { try await saves.flush() }
    await sink.waitForAttempts(2)
    await sink.finishNext()
    try await retry.value
    try await saves.flush()
    let values = await sink.values
    XCTAssertEqual(values, [7, 7])
    XCTAssertFalse(saves.state.hasUnsavedChanges)
  }

  func testCancelledFlushCallerDoesNotAbandonAnOwnedWrite() async throws {
    let sink = ControlledConfigurationSink()
    let saves = try SnapshotSaveQueue<Int> { try await sink.save($0) }
    try saves.enqueue(9)
    let caller = Task { try await saves.flush() }
    await sink.waitForAttempts(1)
    caller.cancel()
    await sink.finishNext()
    try await caller.value
    XCTAssertFalse(saves.state.hasUnsavedChanges)
    XCTAssertEqual(saves.state.committedRevision, 1)
  }
}

private actor ControlledConfigurationSink {
  enum Failure: Error { case expected }
  private(set) var values: [Int] = []
  private var completions: [CheckedContinuation<Void, Error>] = []
  private var observers: [(Int, CheckedContinuation<Void, Never>)] = []
  private var active = 0
  private(set) var maximumActive = 0

  func save(_ value: Int) async throws {
    values.append(value)
    active += 1
    maximumActive = max(maximumActive, active)
    let ready = observers.filter { values.count >= $0.0 }
    observers.removeAll { values.count >= $0.0 }
    for observer in ready { observer.1.resume() }
    defer { active -= 1 }
    try await withCheckedThrowingContinuation { completions.append($0) }
  }

  func waitForAttempts(_ count: Int) async {
    if values.count >= count { return }
    await withCheckedContinuation { observers.append((count, $0)) }
  }

  func finishNext(fail: Bool = false) {
    let completion = completions.removeFirst()
    if fail { completion.resume(throwing: Failure.expected) } else { completion.resume() }
  }
}
