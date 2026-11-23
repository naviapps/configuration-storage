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
        var fake = new FakeNative();
        using var store = await ConfigurationStore.OpenAsync(new(directory, "payload-test.owner"), fake);

        // These are JSON string tokens, including escaped and legacy base64 forms.
        foreach (var (token, expected) in new (string, byte[])[] {
            ("\"/wAB\"", [255, 0, 1]),
            ("\"\\/wAB\"", [255, 0, 1]),
            ("\"\\u002FwAB\"", [255, 0, 1]),
            ("\" A Q I D \\r\\n\\t\"", [1, 2, 3]),
            ("\"YR==\"", [(byte)'a']),
        })
        {
            fake.Reply = "{\"generation\":\"" + FakeNative.Generation + "\",\"payload\":" + token + "}";
            var snapshot = await store.LoadAsync();
            Check(snapshot.Payload!.SequenceEqual(expected), "Snapshot base64 compatibility changed.");
            fake.Reply = RecoveryReply(token, digest);
            var recovery = await ConfigurationRecoveryCopy.ReadAsync(path, expected.Length, fake);
            Check(recovery.Payload.SequenceEqual(expected), "Recovery base64 compatibility or decoded-byte budget changed.");
        }

        foreach (var token in new[] { "false", "0", "[]", "{}", "\"AQID!\"", "\"A===\"", "\"AQID\\u00a0\"" })
        {
            fake.Reply = "{\"generation\":\"" + FakeNative.Generation + "\",\"payload\":" + token + "}";
            await RejectAsync(store.LoadAsync(), typeof(InvalidDataException));
            fake.Reply = RecoveryReply(token, digest);
            await RejectAsync(ConfigurationRecoveryCopy.ReadAsync(path, 3, fake), typeof(InvalidDataException));
        }

        fake.Reply = RecoveryReply("\"AAE=\"", digest);
        await RejectAsync(ConfigurationRecoveryCopy.ReadAsync(path, 1, fake), typeof(InvalidDataException));

        // A native failure remains authoritative even if an unused payload is malformed.
        fake.ReplyCode = 1;
        fake.Reply = "{\"code\":\"ownerMismatch\",\"payload\":false}";
        await FailureAsync(store.LoadAsync(), ErrorCode.OwnerMismatch);
        await FailureAsync(ConfigurationRecoveryCopy.ReadAsync(path, 3, fake), ErrorCode.OwnerMismatch);
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

    private static string RecoveryReply(string token, string digest)
        => "{\"payload\":" + token + ",\"digest\":\"" + digest + "\",\"modifiedAt\":1.25}";
}
