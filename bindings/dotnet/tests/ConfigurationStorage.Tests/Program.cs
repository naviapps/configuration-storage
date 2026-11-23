using System.Text.Json;
using NaviApps.ConfigurationStorage;
using static TestAssertions;

static async Task Reject<T>(Task<T> task, Type type)
{
    try { await task; }
    catch (Exception error) when (type.IsInstanceOfType(error)) { return; }
    throw new System.Diagnostics.UnreachableException("Expected " + type.Name);
}
static async Task Failure<T>(Task<T> task, ErrorCode code)
{
    try { await task; }
    catch (StorageException error) when (error.Failure.Code == code) { return; }
    throw new System.Diagnostics.UnreachableException("Expected storage failure " + code);
}

var directory = Path.Combine(Path.GetTempPath(), "configuration-storage-tests-" + Guid.NewGuid());
var configuration = new Configuration(directory, "sdk-test.owner");
var fake = new FakeNative();
using (var store = await ConfigurationStore.OpenAsync(configuration, fake))
{
    var snapshot = await store.LoadAsync();
    Check(snapshot.IsInitialized && snapshot.Generation == FakeNative.Generation
        && snapshot.Payload!.SequenceEqual(new byte[] { 1, 2, 3 }), "Opaque snapshot decoding.");
    snapshot.Payload![0] = 99;
    Check(snapshot.Payload![0] == 1, "Caller mutated snapshot ownership.");
    var equivalent = new Snapshot(FakeNative.Generation, [1, 2, 3]);
    Check(snapshot == equivalent && snapshot.Equals((object)equivalent)
        && new HashSet<Snapshot> { snapshot }.Contains(equivalent),
        "Equivalent owned snapshots did not compare or hash by value.");
    Check(snapshot != new Snapshot(FakeNative.Generation, [1, 2, 4])
        && snapshot != new Snapshot("other-generation", [1, 2, 3])
        && new Snapshot(null, null) != new Snapshot(FakeNative.Generation, []),
        "Snapshot equality ignored bytes, generation or uninitialized state.");
    var input = new byte[] { 4, 5, 6 };
    using var entered = new ManualResetEventSlim();
    using var release = new ManualResetEventSlim();
    fake.OnDispatch = () => { entered.Set(); if (!release.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException(); };
    var saved = store.SaveAsync(null, input);
    input[0] = 99;
    Check(entered.Wait(TimeSpan.FromSeconds(10)), "Dispatch did not start.");
    release.Set();
    await saved;
    using var command = JsonDocument.Parse(fake.CommandJson!);
    Check(command.RootElement.GetProperty("expectedGeneration").ValueKind == JsonValueKind.Null
        && !command.RootElement.TryGetProperty("payload", out _)
        && fake.CommandPayload!.SequenceEqual(new byte[] { 4, 5, 6 }), "Request bytes not copied or null baseline changed.");
    fake.OnDispatch = null;
    fake.Reply = "{\"generation\":null}";
    fake.ReplyPayload = null;
    snapshot = await store.LoadAsync();
    Check(!snapshot.IsInitialized && snapshot.Payload is null, "Uninitialized null/null was lost.");
    fake.Reply = FakeNative.Initialized;
    fake.ReplyPayload = [];
    snapshot = await store.LoadAsync();
    Check(snapshot.Generation == FakeNative.Generation && snapshot.Payload is { Length: 0 }, "Initialized empty payload was lost.");
    await store.SaveAsync(snapshot.Generation, input);
    using (var saveCommand = JsonDocument.Parse(fake.CommandJson!))
        Check(saveCommand.RootElement.GetProperty("expectedGeneration").GetString() == FakeNative.Generation,
            "Save changed the opaque generation token.");
    fake.Reply = "{\"snapshot\":" + FakeNative.Initialized + ",\"token\":\"opaque-token\",\"modifiedAt\":1700000000.125}";
    fake.ReplyPayload = [1, 2, 3];
    var backup = await store.ReadBackupAsync();
    Check(backup.ModifiedAt == 1700000000.125 && backup.Token == "opaque-token", "Backup identity/time framing.");
    fake.Reply = JsonSerializer.Serialize(new { backup = JsonSerializer.Deserialize<JsonElement>(fake.Reply), token = "opaque-rotation-token" });
    var backupPreparation = await store.PrepareBackupAsync(backup.Snapshot.Generation!);
    Check(backupPreparation.Backup?.Token == backup.Token && backupPreparation.Token == "opaque-rotation-token",
        "Backup preparation confused rotation and restoration tokens.");
    fake.Reply = "{\"snapshot\":" + FakeNative.Initialized + ",\"token\":\"opaque-token\",\"modifiedAt\":1700000000.125}";
    await store.CreateBackupAsync(backup.Snapshot.Generation!, backupPreparation.Token);
    using (var backupCommand = JsonDocument.Parse(fake.CommandJson!))
        Check(backupCommand.RootElement.GetProperty("command").GetString() == "createBackup"
            && backupCommand.RootElement.GetProperty("expectedGeneration").GetString() == FakeNative.Generation
            && backupCommand.RootElement.GetProperty("token").GetString() == backupPreparation.Token,
            "Backup changed the opaque generation or rotation token.");
    fake.Reply = FakeNative.Initialized;
    await store.RestoreBackupAsync(backup.Token);
    Check(fake.CommandJson!.Contains("opaque-token"), "Restore dropped the validated token.");
    await Task.WhenAll(Enumerable.Range(0, 64).Select(_ => store.LoadAsync()));
    Check(fake.MaximumActive == 1, "Operations from one facade overlapped.");
}
Check(fake.StoreDestroys == 1 && fake.ResultDestroys == fake.Dispatches, "Owned native handles leaked.");
using (var config = JsonDocument.Parse(fake.ConfigurationJson!))
    Check(config.RootElement.GetProperty("directory").GetString() == directory
        && config.RootElement.GetProperty("owner").GetString() == "sdk-test.owner"
        && config.RootElement.GetProperty("maximumPayloadBytes").GetInt32() == 16 * 1024 * 1024, "Explicit configuration framing.");

var cancelled = new FakeNative();
using (var source = new CancellationTokenSource())
{
    source.Cancel();
    await Reject(ConfigurationStore.OpenAsync(configuration, cancelled, source.Token), typeof(OperationCanceledException));
    Check(cancelled.Opens == 0, "Pre-cancelled open reached native code.");
}
using (var store = await ConfigurationStore.OpenAsync(configuration, cancelled))
using (var source = new CancellationTokenSource())
{
    source.Cancel();
    await Reject(store.LoadAsync(source.Token), typeof(OperationCanceledException));
    await Reject(store.SaveAsync(null, new byte[] { 1 }, source.Token), typeof(OperationCanceledException));
    Check(cancelled.Dispatches == 0, "Pre-cancelled operation reached native code.");
}

// Dispatch is already accepted when disposal/cancellation occurs. Keep the store alive
// and deliver the completed write, while queued work must not reach native code.
var race = new FakeNative();
using (var started = new ManualResetEventSlim())
using (var finish = new ManualResetEventSlim())
using (var source = new CancellationTokenSource())
{
    race.OnDispatch = () => { started.Set(); if (!finish.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException(); };
    var store = await ConfigurationStore.OpenAsync(configuration, race);
    var accepted = store.SaveAsync(null, new byte[] { 1 }, source.Token);
    Check(started.Wait(TimeSpan.FromSeconds(10)), "Write did not start.");
    using var queuedCancellation = new CancellationTokenSource();
    var queued = store.LoadAsync(queuedCancellation.Token);
    queuedCancellation.Cancel();
    await Reject(queued, typeof(OperationCanceledException));
    var disposedQueue = store.LoadAsync();
    source.Cancel();
    store.Dispose(); store.Dispose();
    Check(race.StoreDestroys == 0, "Disposed store was freed during a native dispatch.");
    finish.Set();
    Check((await accepted).IsInitialized, "Cancellation discarded an accepted write.");
    await Reject(disposedQueue, typeof(ObjectDisposedException));
    await Reject(store.LoadAsync(), typeof(ObjectDisposedException));
    Check(race.StoreDestroys == 1 && race.Dispatches == 1 && race.ResultDestroys == 1, "Disposal/cancellation ownership mismatch.");
}

var openCancellation = new FakeNative();
using (var source = new CancellationTokenSource())
{
    openCancellation.OnOpen = source.Cancel;
    using var store = await ConfigurationStore.OpenAsync(configuration, openCancellation, source.Token);
    Check(openCancellation.Opens == 1, "Cancellation discarded an accepted native open.");
}
foreach (var version in FakeNative.IncompatibleVersions)
{
    var badVersion = new FakeNative { Version = version };
    await Failure(ConfigurationStore.OpenAsync(configuration, badVersion), ErrorCode.UnsupportedAbiVersion);
    Check(badVersion.Opens == 0, "Incompatible ABI opened a store.");
}
var failedOpen = new FakeNative { OpenFailure = """{"code":"ownerMismatch"}""" };
await Failure(ConfigurationStore.OpenAsync(configuration, failedOpen), ErrorCode.OwnerMismatch);
Check(failedOpen.ResultDestroys == 1 && failedOpen.StoreDestroys == 0, "Failed open leaked its error result.");
await Reject(ConfigurationStore.OpenAsync(configuration, new FakeNative { NullOpen = true }), typeof(InvalidDataException));

foreach (var invalid in new[] { configuration with { Directory = "relative" }, configuration with { Directory = directory + '\0' } })
{
    var invalidPath = new FakeNative();
    await Failure(ConfigurationStore.OpenAsync(invalid, invalidPath), ErrorCode.InvalidConfiguration);
    Check(invalidPath.Opens == 0, "An unrepresentable native directory reached open.");
}

// Rust validates owner/limit semantics; the binding forwards values and owns the error result.
var rejectedConfiguration = configuration with { Owner = "invalid owner", MaximumPayloadBytes = -1 };
foreach (var existingOnly in new[] { false, true })
{
    var rejected = new FakeNative { OpenFailure = """{"code":"invalidConfiguration"}""" };
    await Failure(existingOnly
        ? ConfigurationStore.OpenExistingAsync(rejectedConfiguration, rejected)
        : ConfigurationStore.OpenAsync(rejectedConfiguration, rejected), ErrorCode.InvalidConfiguration);
    using var forwarded = JsonDocument.Parse(rejected.ConfigurationJson!);
    Check(forwarded.RootElement.GetProperty("owner").GetString() == rejectedConfiguration.Owner
        && forwarded.RootElement.GetProperty("maximumPayloadBytes").GetInt32() == rejectedConfiguration.MaximumPayloadBytes,
        "The binding normalized configuration semantics before Rust validation.");
    Check(rejected.Opens == (existingOnly ? 0 : 1) && rejected.ExistingOpens == (existingOnly ? 1 : 0)
        && rejected.ResultDestroys == 1 && rejected.StoreDestroys == 0,
        "Native configuration rejection leaked an owner or selected another admission mode.");
}

var errors = new FakeNative { ReplyCode = 1 };
using (var store = await ConfigurationStore.OpenAsync(configuration, errors))
{
    foreach (var code in Enum.GetValues<ErrorCode>().Where(code => code != ErrorCode.UnsupportedAbiVersion))
    {
        string wireCode = char.ToLowerInvariant(code.ToString()[0]) + code.ToString()[1..];
        errors.Reply = JsonSerializer.Serialize(new { code = wireCode, recoveryDirectory = directory });
        try { await store.LoadAsync(); throw new System.Diagnostics.UnreachableException("Failure accepted."); }
        catch (StorageException error)
        {
            Check(error.Failure.Code == code && error.Failure.RecoveryDirectory == directory, "Failure DTO changed.");
        }
    }
    errors.ReplyCode = 0;
    foreach (var malformed in new[] { "{", "null", "{}", "{\"generation\":null}",
        FakeNative.Initialized.Replace(FakeNative.Generation, "") })
    {
        errors.Reply = malformed;
        await Reject(store.LoadAsync(), typeof(InvalidDataException));
    }
    errors.NullDispatch = true;
    await Reject(store.LoadAsync(), typeof(InvalidDataException));
}
Check(errors.ResultDestroys == errors.Dispatches - 1, "Malformed native replies leaked result owners.");

var malformedViews = new FakeNative();
using (var store = await ConfigurationStore.OpenAsync(configuration, malformedViews))
{
    foreach (var rewrite in new Func<ConfigurationStorageResultView, ConfigurationStorageResultView>[] {
        view => new(2, view.Data, view.Length), view => new(0, IntPtr.Zero, view.Length),
        view => new(0, view.Data, 0), view => new(0, view.Data, Wire.MaximumJsonBytes + 1) })
    {
        malformedViews.RewriteView = rewrite;
        await Reject(store.LoadAsync(), typeof(InvalidDataException));
    }
}
Check(malformedViews.ResultDestroys == 4, "Invalid view bounds leaked native result owners.");

var limits = new FakeNative { ReplyPayload = [1] };
using (var store = await ConfigurationStore.OpenAsync(configuration with { MaximumPayloadBytes = 1 }, limits))
{
    await Failure(store.SaveAsync(null, new byte[] { 1, 2 }), ErrorCode.InvalidConfiguration);
    Check(limits.Dispatches == 0, "Oversized input reached native dispatch.");
    await store.SaveAsync(null, new byte[] { 1 });
}

var acceptedFailure = new FakeNative { ReplyCode = 1, Reply = "{\"code\":\"conflict\"}" };
using (var source = new CancellationTokenSource())
using (var store = await ConfigurationStore.OpenAsync(configuration, acceptedFailure))
{
    acceptedFailure.OnDispatch = source.Cancel;
    await Failure(store.SaveAsync(null, new byte[] { 1 }, source.Token), ErrorCode.Conflict);
    Check(acceptedFailure.ResultDestroys == 1, "Late cancellation discarded a native failure result.");
}

await OpenExistingTests.RunManagedAsync();
await LazyOpenTests.RunManagedAsync();
await PayloadDecodingTests.RunManagedAsync();
await RecoveryCopyTests.RunManagedAsync();
await RestoreSnapshotTests.RunManagedAsync();
await BackupRebuildTests.RunManagedAsync();
await BackupRotationTests.RunManagedAsync();
await BindingAcceptanceTests.RunManagedAsync();
await SnapshotCandidateTests.RunManagedAsync();

if (args.Length != 0)
{
    Check(args.Length == 1 && args[0] == "--native", "Pass --native to test the native library copied to the build output.");
    await OpenExistingTests.RunNativeAsync();
    await LazyOpenTests.RunNativeAsync();
    await SnapshotSaveQueueTests.RunAsync();
    await RecoveryCopyTests.RunNativeAsync();
    await RestoreSnapshotTests.RunNativeAsync();
    await BackupRebuildTests.RunNativeAsync();
    await BackupRotationTests.RunNativeAsync();
    await BindingAcceptanceTests.RunNativeAsync();
    await SnapshotCandidateTests.RunNativeAsync();
    await Failure(ConfigurationStore.OpenAsync(rejectedConfiguration), ErrorCode.InvalidConfiguration);
    await Failure(ConfigurationStore.OpenExistingAsync(rejectedConfiguration), ErrorCode.InvalidConfiguration);
    Check(!Directory.Exists(directory), "Rejected configuration created a store before validation.");
    try
    {
        using var store = await ConfigurationStore.OpenAsync(configuration);
        var initial = await store.LoadAsync();
        Check(!initial.IsInitialized && initial.Payload is null, "Real store was not empty.");
        await Failure(store.ReadBackupAsync(), ErrorCode.BackupMissing);
        await Failure(store.PrepareBackupAsync(Guid.NewGuid().ToString()), ErrorCode.NotInitialized);
        var first = await store.SaveAsync(null, new byte[] { 0, 255, 42 });
        var mutableView = first.Payload!;
        mutableView[0] = 99;
        Check(first.Payload![0] == 0 && (await store.LoadAsync()).Payload![0] == 0,
            "Native snapshot bytes must remain owned when a caller mutates its copy.");
        var backupPreparation = await store.PrepareBackupAsync(first.Generation!);
        Check(backupPreparation.Backup is null, "A new store unexpectedly contained a checkpoint.");
        var backup = await store.CreateBackupAsync(first.Generation!, backupPreparation.Token);
        Check((await store.ReadBackupAsync()).Token == backup.Token, "Real backup identity mismatch.");
        var second = await store.SaveAsync(first.Generation, ReadOnlyMemory<byte>.Empty);
        Check(second.Payload is { Length: 0 }, "Real empty bytes became null.");
        await Failure(store.SaveAsync(first.Generation, new byte[] { 1 }), ErrorCode.Conflict);
        await Failure(store.RestoreBackupAsync(backup.Token), ErrorCode.BackupChanged);
        var candidate = await store.ReadBackupAsync();
        Check(candidate.Token != backup.Token && candidate.Snapshot.Payload!.SequenceEqual(first.Payload!),
            "Host must re-read and validate backup bytes after the active selection changes.");
        var restored = await store.RestoreBackupAsync(candidate.Token);
        Check(restored.Generation != first.Generation && restored.Generation != second.Generation
            && restored.Payload!.SequenceEqual(first.Payload!), "Restore did not retain opaque bytes with a new generation.");
        var rotation = await store.PrepareBackupAsync(restored.Generation!);
        await store.CreateBackupAsync(restored.Generation!, rotation.Token);
        await Failure(store.RestoreBackupAsync(candidate.Token), ErrorCode.BackupChanged);
        var observations = await Task.WhenAll(Enumerable.Range(0, 16).Select(_ => store.LoadAsync()));
        Check(observations.All(value => value.Generation == restored.Generation), "Serialized native reads returned inconsistent snapshots.");
        using var preCancelled = new CancellationTokenSource();
        preCancelled.Cancel();
        await Reject(store.SaveAsync(restored.Generation, new byte[] { 1 }, preCancelled.Token), typeof(OperationCanceledException));
        Check((await store.LoadAsync()).Generation == restored.Generation, "Pre-cancelled native write changed storage.");
        await Failure(ConfigurationStore.OpenAsync(configuration with { Owner = "different-owner" }), ErrorCode.OwnerMismatch);
    }
    finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
    Console.WriteLine("Native disposable-directory snapshot/conflict/backup/restore/owner checks passed.");
}
Console.WriteLine("Managed serialization, copy ownership, ABI failures, cancellation and disposal checks passed.");
