using System.Text.Json;
using NaviApps.ConfigurationStorage;
using static TestAssertions;

static class BackupRebuildTests
{
    private const string BackupReply = "{\"snapshot\":" + FakeNative.Initialized
        + ",\"token\":\"opaque-backup-token\",\"modifiedAt\":1700000000.125}";

    internal static async Task RunManagedAsync()
    {
        var configuration = new Configuration(Path.Combine(Path.GetTempPath(), "backup-rebuild-fake-" + Guid.NewGuid()), "backup.rebuild.test");
        const string generation = "opaque-generation/\"\\\n";
        const string token = "opaque-review/\"\\\n";
        var fake = new FakeNative { Reply = JsonSerializer.Serialize(token), ReplyPayload = null };
        using (var store = await ConfigurationStore.OpenAsync(configuration, fake))
        {
            Check(await store.PrepareBackupRebuildAsync(generation) == token, "Backup review did not decode an opaque JSON string token.");
            using (var command = JsonDocument.Parse(fake.CommandJson!))
            {
                var root = command.RootElement;
                Check(root.EnumerateObject().Count() == 2 && root.GetProperty("command").GetString() == "prepareBackupRebuild"
                    && root.GetProperty("expectedGeneration").GetString() == generation, "Backup review changed the caller's generation.");
            }
            fake.Reply = BackupReply;
            fake.ReplyPayload = [1, 2, 3];
            var rebuilt = await store.RebuildBackupAsync(generation, token);
            Check(rebuilt.Snapshot.Generation == FakeNative.Generation
                && rebuilt.Snapshot.Payload!.SequenceEqual(new byte[] { 1, 2, 3 })
                && rebuilt.Token == "opaque-backup-token" && rebuilt.ModifiedAt == 1700000000.125,
                "Rebuild changed backup bytes, identity or fractional timestamp.");
            using (var command = JsonDocument.Parse(fake.CommandJson!))
            {
                var root = command.RootElement;
                Check(root.EnumerateObject().Count() == 3 && root.GetProperty("command").GetString() == "rebuildBackup"
                    && root.GetProperty("expectedGeneration").GetString() == generation
                    && root.GetProperty("token").GetString() == token, "Rebuild dropped the explicit generation or raw review token.");
            }

            foreach (var malformed in new[] { "null", "{}", "[]", "false", "1", "\"\"", "{\"token\":\"nested\"}" })
            {
                fake.Reply = malformed;
                await RejectAsync(store.PrepareBackupRebuildAsync(generation), typeof(InvalidDataException));
            }
            foreach (var malformed in new[] { "null", "{}", FakeNative.Initialized, BackupReply.Replace("opaque-backup-token", "") })
            {
                fake.Reply = malformed;
                await RejectAsync(store.RebuildBackupAsync(generation, token), typeof(InvalidDataException));
            }
            using var cancelled = new CancellationTokenSource();
            cancelled.Cancel();
            var dispatches = fake.Dispatches;
            await RejectAsync(store.PrepareBackupRebuildAsync(generation, cancelled.Token), typeof(OperationCanceledException));
            await RejectAsync(store.RebuildBackupAsync(generation, token, cancelled.Token), typeof(OperationCanceledException));
            Check(fake.Dispatches == dispatches, "Pre-cancelled backup reconstruction reached native dispatch.");

            fake.OnDispatch = () =>
            {
                using var command = JsonDocument.Parse(fake.CommandJson!);
                var name = command.RootElement.GetProperty("command").GetString();
                fake.Reply = name switch
                {
                    "prepareBackupRebuild" => JsonSerializer.Serialize(token),
                    "prepareBackup" => "{\"backup\":null,\"token\":\"opaque-rotation-token\"}",
                    "load" => FakeNative.Initialized,
                    _ => BackupReply
                };
                fake.ReplyPayload = name is "prepareBackupRebuild" or "prepareBackup" ? null : [1, 2, 3];
            };
            await Task.WhenAll(Enumerable.Range(0, 16).SelectMany(_ => new Task[] {
                store.PrepareBackupRebuildAsync(generation), store.RebuildBackupAsync(generation, token),
                store.PrepareBackupAsync(generation), store.CreateBackupAsync(generation, "opaque-rotation-token"),
                store.ReadBackupAsync(), store.LoadAsync() }));
            Check(fake.MaximumActive == 1, "Backup reconstruction bypassed the existing store serialization gate.");
        }
        Check(fake.StoreDestroys == 1 && fake.ResultDestroys == fake.Dispatches, "Backup reconstruction leaked owned native handles.");

        foreach (var preparing in new[] { true, false })
        {
            using var source = new CancellationTokenSource();
            var accepted = new FakeNative
            {
                Reply = preparing ? JsonSerializer.Serialize(token) : BackupReply,
                ReplyPayload = preparing ? null : [1, 2, 3],
                OnDispatch = source.Cancel
            };
            using var store = await ConfigurationStore.OpenAsync(configuration, accepted);
            if (preparing) Check(await store.PrepareBackupRebuildAsync(generation, source.Token) == token, "Late cancellation discarded review completion.");
            else Check((await store.RebuildBackupAsync(generation, token, source.Token)).Snapshot.IsInitialized, "Late cancellation discarded a confirmed rebuild.");
            Check(accepted.ResultDestroys == 1, "Late cancellation leaked a backup reconstruction result.");
        }
    }

    internal static async Task RunNativeAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "configuration-backup-rebuild-" + Guid.NewGuid());
        Directory.CreateDirectory(directory);
        try
        {
            var configuration = new Configuration(directory, "backup.rebuild.test");
            using var store = await ConfigurationStore.OpenAsync(configuration);
            var first = await store.SaveAsync(null, new byte[] { 0, 255, 1 });
            var preparation = await store.PrepareBackupAsync(first.Generation!);
            await store.CreateBackupAsync(first.Generation!, preparation.Token);
            var backup = await BackupPathAsync(directory);
            var latest = await store.SaveAsync(first.Generation, new byte[] { 128, 0, 254 });
            var generation = latest.Generation!;
            var damage = "damaged checkpoint"u8.ToArray();
            await File.WriteAllBytesAsync(backup, damage);

            try
            {
                await store.PrepareBackupAsync(generation);
                throw new InvalidOperationException("Normal preparation accepted a damaged backup.");
            }
            catch (StorageException error)
            {
                Check(error.Failure.Code == ErrorCode.InvalidStoredData && error.Failure.RecoveryDirectory is not null,
                    "Backup refusal lost its raw recovery capture.");
                var capture = Path.Combine(error.Failure.RecoveryDirectory!, Path.GetFileName(backup));
                Check((await File.ReadAllBytesAsync(capture)).SequenceEqual(damage), "Capture lost native backup bytes.");
            }

            var review = await store.PrepareBackupRebuildAsync(generation);
            var rebuilt = await store.RebuildBackupAsync(generation, review);
            Check(rebuilt.Snapshot.Generation == latest.Generation && rebuilt.Snapshot.Payload!.SequenceEqual(latest.Payload!),
                "Rebuild did not return the current native snapshot.");
            using (var reopened = await ConfigurationStore.OpenAsync(configuration))
            {
                var read = await reopened.ReadBackupAsync();
                Check(read.Token == rebuilt.Token && read.ModifiedAt == rebuilt.ModifiedAt
                    && read.Snapshot.Generation == rebuilt.Snapshot.Generation
                    && read.Snapshot.Payload!.SequenceEqual(rebuilt.Snapshot.Payload!),
                    "Reopened store did not project the selected backup.");
            }
            await FailureAsync(store.RebuildBackupAsync(generation, review), ErrorCode.BackupChanged);
        }
        finally { Directory.Delete(directory, true); }
        Console.WriteLine("Native backup rebuild roundtrip, expired review, and raw recovery capture projection passed.");
    }

    private static async Task<string> BackupPathAsync(string directory)
    {
        var marker = Path.Combine(directory, "active-backup");
        var backups = Path.Combine(directory, "Backups");
        if (File.Exists(marker))
        {
            var name = await File.ReadAllTextAsync(marker);
            Check(Guid.TryParseExact(name, "D", out var id) && id.ToString("D") == name, "Invalid backup selector in test fixture.");
            return Path.Combine(backups, name + ".sqlite");
        }
        Check(!Directory.EnumerateFiles(backups, "*.sqlite").Any(path => Guid.TryParseExact(Path.GetFileNameWithoutExtension(path), "D", out _)),
            "A missing selector must not fall back to a legacy backup while UUID checkpoints exist.");
        return Path.Combine(backups, "snapshot.sqlite");
    }
}
