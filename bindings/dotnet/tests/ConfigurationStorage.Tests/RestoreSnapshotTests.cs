using System.Text.Json;
using NaviApps.ConfigurationStorage;
using static TestAssertions;

static class RestoreSnapshotTests
{
    internal static async Task RunManagedAsync()
    {
        var configuration = new Configuration(Path.Combine(Path.GetTempPath(), "restore-fake-" + Guid.NewGuid()), "restore.test");
        const string token = "opaque-restore-token";
        var fake = new FakeNative { Reply = JsonSerializer.Serialize(token), ReplyPayload = null };
        using (var store = await ConfigurationStore.OpenAsync(configuration, fake))
        {
            var payload = new byte[] { 0, 255, 1 };
            using var entered = new ManualResetEventSlim();
            using var release = new ManualResetEventSlim();
            fake.OnDispatch = () => { entered.Set(); if (!release.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException(); };
            var preparing = store.PrepareRestoreAsync(payload);
            payload[0] = 99;
            try { Check(entered.Wait(TimeSpan.FromSeconds(10)), "Restore preparation did not start."); }
            finally { release.Set(); }
            Check(await preparing == token, "Prepare did not decode an opaque JSON string token.");
            using (var command = JsonDocument.Parse(fake.CommandJson!))
                Check(command.RootElement.GetProperty("command").GetString() == "prepareRestore"
                    && !command.RootElement.TryGetProperty("payload", out _)
                    && fake.CommandPayload!.SequenceEqual(new byte[] { 0, 255, 1 }), "Prepare did not capture caller bytes.");

            entered.Reset();
            release.Reset();
            payload[0] = 0;
            fake.Reply = FakeNative.Initialized;
            fake.ReplyPayload = [1, 2, 3];
            var restoring = store.RestoreSnapshotAsync(payload, token);
            payload[0] = 99;
            try { Check(entered.Wait(TimeSpan.FromSeconds(10)), "Snapshot restore did not start."); }
            finally { release.Set(); }
            Check((await restoring).Generation == FakeNative.Generation, "Restore snapshot decoding changed.");
            using (var command = JsonDocument.Parse(fake.CommandJson!))
                Check(command.RootElement.GetProperty("command").GetString() == "restoreSnapshot"
                    && !command.RootElement.TryGetProperty("payload", out _)
                    && fake.CommandPayload!.SequenceEqual(new byte[] { 0, 255, 1 })
                    && command.RootElement.GetProperty("token").GetString() == token, "Restore changed caller bytes or the opaque token.");

            fake.OnDispatch = null;
            foreach (var malformed in new[] { "null", "{}", "[]", "false", "1", "\"\"", "{\"token\":\"nested\"}" })
            {
                fake.Reply = malformed;
                await RejectAsync(store.PrepareRestoreAsync(ReadOnlyMemory<byte>.Empty), typeof(InvalidDataException));
            }
            fake.Reply = "null";
            await RejectAsync(store.RestoreSnapshotAsync(payload, token), typeof(InvalidDataException));

            // Mixed old/new operations share the same native serialization gate.
            fake.OnDispatch = () =>
            {
                using var command = JsonDocument.Parse(fake.CommandJson!);
                var preparing = command.RootElement.GetProperty("command").GetString() == "prepareRestore";
                fake.Reply = preparing ? JsonSerializer.Serialize(token) : FakeNative.Initialized;
                fake.ReplyPayload = preparing ? null : [1, 2, 3];
            };
            await Task.WhenAll(Enumerable.Range(0, 32).SelectMany(_ => new Task[] {
                store.PrepareRestoreAsync(payload), store.RestoreSnapshotAsync(payload, token), store.LoadAsync() }));
            Check(fake.MaximumActive == 1, "New restore operations bypassed store serialization.");
        }
        Check(fake.StoreDestroys == 1 && fake.ResultDestroys == fake.Dispatches, "Restore operations leaked native owners.");

        var limits = new FakeNative();
        using (var store = await ConfigurationStore.OpenAsync(configuration with { MaximumPayloadBytes = 1 }, limits))
        {
            await FailureAsync(store.PrepareRestoreAsync(new byte[] { 1, 2 }), ErrorCode.InvalidConfiguration);
            await FailureAsync(store.RestoreSnapshotAsync(new byte[] { 1, 2 }, token), ErrorCode.InvalidConfiguration);
            using var source = new CancellationTokenSource();
            source.Cancel();
            await RejectAsync(store.PrepareRestoreAsync(new byte[] { 1 }, source.Token), typeof(OperationCanceledException));
            await RejectAsync(store.RestoreSnapshotAsync(new byte[] { 1 }, token, source.Token), typeof(OperationCanceledException));
            Check(limits.Dispatches == 0, "Rejected restore input reached native dispatch.");
        }

        var race = new FakeNative();
        using (var entered = new ManualResetEventSlim())
        using (var release = new ManualResetEventSlim())
        using (var source = new CancellationTokenSource())
        using (var queuedCancellation = new CancellationTokenSource())
        using (var store = await ConfigurationStore.OpenAsync(configuration, race))
        {
            race.OnDispatch = () => { entered.Set(); if (!release.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException(); };
            var accepted = store.RestoreSnapshotAsync(new byte[] { 1 }, token, source.Token);
            Task<string>? disposedQueue = null;
            try
            {
                Check(entered.Wait(TimeSpan.FromSeconds(10)), "Restore lifetime race did not start.");
                var cancelledQueue = store.PrepareRestoreAsync(new byte[] { 1 }, queuedCancellation.Token);
                queuedCancellation.Cancel();
                await RejectAsync(cancelledQueue, typeof(OperationCanceledException));
                disposedQueue = store.PrepareRestoreAsync(new byte[] { 1 });
                source.Cancel();
                store.Dispose();
                Check(race.StoreDestroys == 0, "Disposal freed an accepted restore's owner.");
            }
            finally { release.Set(); }
            Check((await accepted).IsInitialized, "Late cancellation discarded a committed restore.");
            await RejectAsync(disposedQueue!, typeof(ObjectDisposedException));
            await RejectAsync(store.RestoreSnapshotAsync(new byte[] { 1 }, token), typeof(ObjectDisposedException));
            Check(race.StoreDestroys == 1 && race.Dispatches == 1 && race.ResultDestroys == 1,
                "Restore cancellation or disposal bypassed existing store lifetime contracts.");
        }
        using (var source = new CancellationTokenSource())
        {
            var accepted = new FakeNative { Reply = JsonSerializer.Serialize(token), ReplyPayload = null, OnDispatch = source.Cancel };
            using var store = await ConfigurationStore.OpenAsync(configuration, accepted);
            Check(await store.PrepareRestoreAsync(new byte[] { 1 }, source.Token) == token && accepted.ResultDestroys == 1,
                "Late cancellation discarded an accepted restore preparation.");
        }
        using (var source = new CancellationTokenSource())
        {
            var rejected = new FakeNative { ReplyCode = 1, Reply = "{\"code\":\"restoreChanged\"}", OnDispatch = source.Cancel };
            using var store = await ConfigurationStore.OpenAsync(configuration, rejected);
            await FailureAsync(store.RestoreSnapshotAsync(new byte[] { 1 }, token, source.Token), ErrorCode.RestoreChanged);
            Check(rejected.ResultDestroys == 1, "Late cancellation discarded a native restore rejection.");
        }
    }

    internal static async Task RunNativeAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "configuration-restore-snapshot-" + Guid.NewGuid());
        Directory.CreateDirectory(directory);
        try
        {
            // The copy has no owner identity; it can be validated and restored by a new store.
            var path = Path.Combine(directory, "external.copy");
            var payload = new byte[] { 0, 255, 128, 1, 0, 254 };
            await ConfigurationRecoveryCopy.WriteAsync(path, payload);
            var candidate = await ConfigurationRecoveryCopy.ReadAsync(path);
            var configuration = new Configuration(Path.Combine(directory, "store"), "new.restore.owner", 1024);
            using var store = await ConfigurationStore.OpenAsync(configuration);
            Check(!(await store.LoadAsync()).IsInitialized, "Native restore fixture was already initialized.");
            var token = await store.PrepareRestoreAsync(candidate.Payload);
            Check(token.Length != 0, "The native restore token was empty.");
            Check(!(await store.LoadAsync()).IsInitialized, "Preparing restoration initialized storage.");
            var changed = payload.ToArray();
            changed[0] ^= 1;
            await FailureAsync(store.RestoreSnapshotAsync(changed, token), ErrorCode.RestoreChanged);
            Check(!(await store.LoadAsync()).IsInitialized, "A rejected restore initialized the store.");
            var restored = await store.RestoreSnapshotAsync(candidate.Payload, token);
            Check(restored.IsInitialized && restored.Payload!.SequenceEqual(payload), "Snapshot restore lost opaque bytes.");
            using var reopened = await ConfigurationStore.OpenAsync(configuration);
            var loaded = await reopened.LoadAsync();
            Check(loaded.Generation == restored.Generation && loaded.Payload!.SequenceEqual(payload),
                "The restored snapshot did not survive reopening the store.");
        }
        finally { Directory.Delete(directory, true); }
        Console.WriteLine("Native snapshot token rejection, opaque restore, and reopened snapshot checks passed.");
    }
}
