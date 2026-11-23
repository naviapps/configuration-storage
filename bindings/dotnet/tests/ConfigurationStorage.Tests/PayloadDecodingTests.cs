using NaviApps.ConfigurationStorage;
using System.Text.Json;
using static TestAssertions;

static class PayloadDecodingTests
{
    internal static async Task RunManagedAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "payload-decoding-" + Guid.NewGuid());
        var path = Path.Combine(directory, "portable.copy");
        var digest = new string('a', 64);
        var recoveryMetadata = "{\"digest\":\"" + digest + "\",\"modifiedAt\":1.25";
        var fake = new FakeNative();
        using var store = await ConfigurationStore.OpenAsync(new(directory, "payload-test.owner"), fake);

        foreach (var bytes in new byte[][] { [0, 255, 128], [] })
        {
            fake.Reply = FakeNative.Initialized;
            fake.ReplyPayload = bytes;
            var snapshot = await store.LoadAsync();
            Check(snapshot.Payload!.SequenceEqual(bytes), "The snapshot changed binary or empty bytes.");
            fake.Reply = recoveryMetadata + "}";
            var recovery = await ConfigurationRecoveryCopy.ReadAsync(path, 3, fake);
            Check(recovery.Payload.SequenceEqual(bytes), "The recovery copy changed binary or empty bytes.");
        }

        fake.ReplyPayload = [1];
        fake.Reply = recoveryMetadata + ",\"payload\":\"AQ==\"}";
        await RejectAsync(ConfigurationRecoveryCopy.ReadAsync(path, 3, fake), typeof(InvalidDataException));
        fake.Reply = FakeNative.Initialized[..^1] + ",\"payload\":\"AQ==\"}";
        await RejectAsync(store.LoadAsync(), typeof(InvalidDataException));

        fake.Reply = FakeNative.Initialized;
        fake.RewriteView = view => new(view.Code, view.Data, view.Length, IntPtr.Zero, view.PayloadLength);
        await RejectAsync(store.LoadAsync(), typeof(InvalidDataException));
        fake.RewriteView = view => new(view.Code, view.Data, view.Length, view.Payload, nuint.MaxValue);
        await RejectAsync(store.LoadAsync(), typeof(InvalidDataException));
        fake.RewriteView = null;
        fake.ReplyPayload = null;
        await RejectAsync(store.LoadAsync(), typeof(InvalidDataException));
        Check(fake.ResultDestroys == fake.Dispatches + fake.RecoveryCopyDispatches,
            "Payload projection leaked or double-freed a native reply.");

        await CheckSnapshotLimits(directory);
    }

    private static async Task CheckSnapshotLimits(string directory)
    {
        const string token = "reviewed";
        const string backup = "{\"snapshot\":" + FakeNative.Initialized
            + ",\"token\":\"reviewed\",\"modifiedAt\":1.25}";
        foreach (var limit in new[] { 3, 2 })
        {
            var fake = new FakeNative();
            fake.OnDispatch = () =>
            {
                using var request = JsonDocument.Parse(fake.CommandJson!);
                fake.Reply = request.RootElement.GetProperty("command").GetString() switch
                {
                    "readBackup" or "createBackup" or "rebuildBackup" => backup,
                    "prepareBackup" => "{\"backup\":" + backup + ",\"token\":\"rotation\"}",
                    "readSnapshotCandidate" => "{\"id\":\"opaque\",\"source\":\"generation\",\"isSelected\":true,"
                        + "\"snapshot\":" + FakeNative.Initialized + ",\"modifiedAt\":1.25}",
                    _ => FakeNative.Initialized
                };
            };
            using var store = await ConfigurationStore.OpenAsync(new(directory, "payload-test.owner", limit), fake);
            Func<Task>[] operations = [
                () => store.LoadAsync(),
                () => store.SaveAsync(null, new byte[] { 1 }),
                () => store.ReadSnapshotCandidateAsync("opaque"),
                () => store.ReadBackupAsync(),
                () => store.CreateBackupAsync(FakeNative.Generation, token),
                () => store.RebuildBackupAsync(FakeNative.Generation, token),
                () => store.PrepareBackupAsync(FakeNative.Generation),
                () => store.RestoreBackupAsync(token),
                () => store.RestoreSnapshotAsync(new byte[] { 1 }, token),
            ];
            foreach (var operation in operations)
            {
                if (limit == 3) await operation();
                else await RejectAsync(operation(), typeof(InvalidDataException));
            }
            Check(fake.ResultDestroys == fake.Dispatches, "Payload limit rejection leaked or double-freed a native reply.");
        }
    }
}
