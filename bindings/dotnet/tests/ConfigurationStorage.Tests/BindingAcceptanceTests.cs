using System.Text.Json;
using NaviApps.ConfigurationStorage;
using static TestAssertions;

static class BindingAcceptanceTests
{
    private const double PreEpochSeconds = -1.25;

    internal static async Task RunManagedAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "binding-dates-fake-" + Guid.NewGuid());
        var fake = new FakeNative();
        using (var store = await ConfigurationStore.OpenAsync(new Configuration(directory, "binding.acceptance.test"), fake))
        {
            // Signed Unix seconds also cover the Windows file-time epoch (1601-01-01 UTC).
            foreach (var seconds in new[] { 1700000000.125, 0, PreEpochSeconds, -11644473600.0 })
            {
                fake.Reply = JsonSerializer.Serialize(new
                {
                    snapshot = new { generation = FakeNative.Generation, payload = "AQID" },
                    token = "opaque-backup-token",
                    modifiedAt = seconds
                });
                var backup = await store.ReadBackupAsync();
                Check(backup.ModifiedAt == seconds && backup.Token == "opaque-backup-token"
                    && backup.Snapshot.Payload!.SequenceEqual(new byte[] { 1, 2, 3 }),
                    "Backup decoding rejected or changed signed Unix seconds or opaque bytes.");
                fake.Reply = JsonSerializer.Serialize(new { payload = "AQID", digest = new string('a', 64), modifiedAt = seconds });
                var copy = await ConfigurationRecoveryCopy.ReadAsync(Path.Combine(directory, "portable.copy"), 1024, fake);
                Check(copy.ModifiedAt == seconds && copy.Payload.SequenceEqual(new byte[] { 1, 2, 3 }),
                    "Recovery-copy decoding rejected or changed signed Unix seconds or opaque bytes.");
            }
            foreach (var seconds in new[] { "1e999", "-1e999", "null", "\"-1.25\"" })
            {
                fake.Reply = "{\"snapshot\":" + FakeNative.Initialized
                    + ",\"token\":\"opaque-backup-token\",\"modifiedAt\":" + seconds + "}";
                await RejectAsync(store.ReadBackupAsync(), typeof(InvalidDataException));
            }
        }
        Check(fake.StoreDestroys == 1 && fake.ResultDestroys == fake.Dispatches + fake.RecoveryCopyDispatches,
            "Signed timestamp decoding leaked native result owners.");
    }

    internal static async Task RunNativeAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "configuration-binding-acceptance-" + Guid.NewGuid());
        Directory.CreateDirectory(directory);
        try
        {
            await PreEpochBackupAsync(Path.Combine(directory, "backup"));
            await PreEpochRecoveryCopyAsync(directory);
        }
        finally { Directory.Delete(directory, true); }
        Console.WriteLine("Native signed backup and recovery-copy modification times passed.");
    }

    private static async Task PreEpochBackupAsync(string directory)
    {
        var configuration = new Configuration(directory, "binding.acceptance.test");
        using var store = await ConfigurationStore.OpenAsync(configuration);
        var original = await store.SaveAsync(null, new byte[] { 0, 255, 128, 0, 254 });
        var preparation = await store.PrepareBackupAsync(original.Generation!);
        await store.CreateBackupAsync(original.Generation!, preparation.Token);
        var marker = Path.Combine(directory, "active-backup");
        var path = File.Exists(marker)
            ? Path.Combine(directory, "Backups", (await File.ReadAllTextAsync(marker)) + ".sqlite")
            : Path.Combine(directory, "Backups", "snapshot.sqlite");
        SetPreEpochModificationTime(path);

        var candidate = await store.ReadBackupAsync();
        Check(candidate.Snapshot.Generation == original.Generation && candidate.Snapshot.Payload!.SequenceEqual(original.Payload!)
            && candidate.Token.Length > 0 && candidate.ModifiedAt == PreEpochSeconds,
            "Native backup read lost pre-epoch fractional seconds, payload or identity.");
        var rotation = await store.PrepareBackupAsync(original.Generation!);
        Check(rotation.Backup?.ModifiedAt == PreEpochSeconds, "Backup preparation changed its signed timestamp.");
        var reused = await store.CreateBackupAsync(original.Generation!, rotation.Token);
        Check(reused.Token == candidate.Token && reused.ModifiedAt == PreEpochSeconds,
            "Reusing an unchanged backup rejected or rewrote its pre-epoch timestamp.");
    }

    private static async Task PreEpochRecoveryCopyAsync(string directory)
    {
        var path = Path.Combine(directory, "portable.copy");
        var payload = new byte[] { 0, 255, 128, 0, 254 };
        await ConfigurationRecoveryCopy.WriteAsync(path, payload);
        var original = await ConfigurationRecoveryCopy.ReadAsync(path);
        var bytes = await File.ReadAllBytesAsync(path);
        SetPreEpochModificationTime(path);
        var copy = await ConfigurationRecoveryCopy.ReadAsync(path);
        var repeated = await ConfigurationRecoveryCopy.ReadAsync(path);
        Check(copy.Payload.SequenceEqual(payload) && copy.Digest == original.Digest && copy.ModifiedAt == PreEpochSeconds
            && repeated.Digest == copy.Digest && repeated.ModifiedAt == PreEpochSeconds,
            "Native recovery-copy read lost pre-epoch fractional seconds or changed its digest or payload.");
        var configuration = new Configuration(Path.Combine(directory, "copy-restore"), "binding.acceptance.test");
        using var store = await ConfigurationStore.OpenAsync(configuration);
        var token = await store.PrepareRestoreAsync(copy.Payload);
        var restored = await store.RestoreSnapshotAsync(copy.Payload, token);
        using var reopened = await ConfigurationStore.OpenAsync(configuration);
        var loaded = await reopened.LoadAsync();
        Check(restored.IsInitialized && restored.Payload!.SequenceEqual(payload) && loaded.Generation == restored.Generation
            && loaded.Payload!.SequenceEqual(payload) && (await File.ReadAllBytesAsync(path)).SequenceEqual(bytes),
            "Pre-epoch recovery-copy roundtrip changed source bytes or failed to persist the restored snapshot.");
    }

    private static void SetPreEpochModificationTime(string path)
    {
        // UTC avoids local timezone conversion; 1969 is within Windows' supported file-time range.
        var date = DateTime.UnixEpoch.AddTicks(-12_500_000);
        File.SetLastWriteTimeUtc(path, date);
        Check(File.GetLastWriteTimeUtc(path) == date, "The owned fixture file did not retain the requested -1.25-second mtime.");
    }
}
