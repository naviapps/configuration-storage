using System.Text.Json;
using NaviApps.ConfigurationStorage;
using static TestAssertions;

static class BackupRotationTests
{
    private const string Generation = "opaque-generation/\"\\\n";
    private const string RotationToken = "opaque-rotation/\"\\\n";
    private const string RestoreToken = "opaque-restore-token";
    private const string BackupReply = "{\"snapshot\":" + FakeNative.Initialized
        + ",\"token\":\"" + RestoreToken + "\",\"modifiedAt\":1700000000.125}";

    private static string PreparationReply(bool present)
        => "{\"backup\":" + (present ? BackupReply : "null")
            + ",\"token\":" + JsonSerializer.Serialize(RotationToken) + "}";

    internal static async Task RunManagedAsync()
    {
        var signatures = typeof(ConfigurationStore).GetMethods()
            .Where(method => method.Name == nameof(ConfigurationStore.CreateBackupAsync)).ToArray();
        Check(signatures.Length == 1, "An unguarded backup-creation overload remains public.");
        var parameters = signatures[0].GetParameters();
        Check(parameters.Length == 3 && parameters[1].ParameterType == typeof(string)
            && !parameters[1].IsOptional, "An unguarded backup-creation overload remains public.");
        var configuration = new Configuration(
            Path.Combine(Path.GetTempPath(), "backup-rotation-fake-" + Guid.NewGuid()), "backup.rotation.test");
        var fake = new FakeNative { Reply = PreparationReply(false), ReplyPayload = null };
        using (var store = await ConfigurationStore.OpenAsync(configuration, fake))
        {
            var absent = await store.PrepareBackupAsync(Generation);
            Check(absent.Backup is null && absent.Token == RotationToken,
                "Preparation did not preserve explicit backup absence and its opaque rotation token.");
            using (var command = JsonDocument.Parse(fake.CommandJson!))
            {
                var root = command.RootElement;
                Check(root.EnumerateObject().Count() == 2 && root.GetProperty("command").GetString() == "prepareBackup"
                    && root.GetProperty("expectedGeneration").GetString() == Generation,
                    "Backup preparation changed the caller's explicit generation or request shape.");
            }
            fake.Reply = PreparationReply(true);
            fake.ReplyPayload = [1, 2, 3];
            var present = await store.PrepareBackupAsync(Generation);
            Check(present.Token == RotationToken && present.Backup is { } backup
                && backup.Token == RestoreToken && backup.Token != present.Token
                && backup.ModifiedAt == 1700000000.125
                && backup.Snapshot.Payload!.SequenceEqual(new byte[] { 1, 2, 3 }),
                "Preparation confused nested restore identity, payload, timestamp or outer rotation authorization.");
            var mutable = present.Backup!.Snapshot.Payload!;
            mutable[0] = 99;
            Check(present.Backup.Snapshot.Payload![0] == 1, "Prepared backup bytes were borrowed from a mutable view.");
            fake.Reply = BackupReply;
            var created = await store.CreateBackupAsync(Generation, present.Token);
            Check(created.Token == RestoreToken, "Backup creation returned the rotation token as restoration identity.");
            using (var command = JsonDocument.Parse(fake.CommandJson!))
            {
                var root = command.RootElement;
                Check(root.EnumerateObject().Count() == 3 && root.GetProperty("command").GetString() == "createBackup"
                    && root.GetProperty("expectedGeneration").GetString() == Generation
                    && root.GetProperty("token").GetString() == RotationToken,
                    "Backup creation dropped or normalized the explicit generation/rotation token.");
            }

            foreach (var malformed in new[] {
                "null", "{}", "[]", "false", "1", "\"rotation\"",
                "{\"token\":\"rotation\"}", "{\"backup\":null}",
                "{\"backup\":null,\"token\":null}", "{\"backup\":null,\"token\":\"\"}",
                "{\"backup\":null,\"token\":1}", "{\"backup\":[],\"token\":\"rotation\"}",
                "{\"backup\":" + FakeNative.Initialized + ",\"token\":\"rotation\"}",
                "{\"backup\":" + BackupReply.Replace(RestoreToken, "") + ",\"token\":\"rotation\"}",
                "{\"backup\":" + BackupReply.Replace(FakeNative.Initialized, "{\"generation\":null}") + ",\"token\":\"rotation\"}",
                "{\"backup\":" + BackupReply.Replace("1700000000.125", "1e999") + ",\"token\":\"rotation\"}",
                "{\"backup\":null,\"backup\":null,\"token\":\"rotation\"}",
                "{\"backup\":null,\"token\":\"first\",\"token\":\"second\"}" })
            {
                fake.Reply = malformed;
                await RejectAsync(store.PrepareBackupAsync(Generation), typeof(InvalidDataException));
            }
            fake.ReplyCode = 1;
            fake.Reply = "{\"code\":\"backupChanged\"}";
            await FailureAsync(store.CreateBackupAsync(Generation, RestoreToken), ErrorCode.BackupChanged);
            using (var command = JsonDocument.Parse(fake.CommandJson!))
                Check(command.RootElement.GetProperty("token").GetString() == RestoreToken,
                    "The binding interpreted or replaced an opaque token before Rust validated its purpose.");
            fake.ReplyCode = 0;

            using var cancelled = new CancellationTokenSource();
            cancelled.Cancel();
            var dispatches = fake.Dispatches;
            await RejectAsync(store.PrepareBackupAsync(Generation, cancelled.Token), typeof(OperationCanceledException));
            await RejectAsync(store.CreateBackupAsync(Generation, RotationToken, cancelled.Token), typeof(OperationCanceledException));
            Check(fake.Dispatches == dispatches, "Pre-cancelled backup preparation or rotation reached native code.");

            fake.OnDispatch = () =>
            {
                using var command = JsonDocument.Parse(fake.CommandJson!);
                fake.Reply = command.RootElement.GetProperty("command").GetString() switch
                {
                    "prepareBackup" => PreparationReply(true),
                    "load" => FakeNative.Initialized,
                    _ => BackupReply
                };
            };
            await Task.WhenAll(Enumerable.Range(0, 16).SelectMany(_ => new Task[] {
                store.PrepareBackupAsync(Generation), store.CreateBackupAsync(Generation, RotationToken),
                store.ReadBackupAsync(), store.LoadAsync() }));
            Check(fake.MaximumActive == 1, "Backup preparation or rotation bypassed the store serialization gate.");
        }
        Check(fake.StoreDestroys == 1 && fake.ResultDestroys == fake.Dispatches,
            "Backup preparation, malformed replies or rotation leaked native owners.");

        foreach (var preparing in new[] { true, false })
        {
            using var cancellation = new CancellationTokenSource();
            var accepted = new FakeNative
            {
                Reply = preparing ? PreparationReply(false) : BackupReply,
                ReplyPayload = preparing ? null : [1, 2, 3],
                OnDispatch = cancellation.Cancel
            };
            using var store = await ConfigurationStore.OpenAsync(configuration, accepted);
            if (preparing)
                Check((await store.PrepareBackupAsync(Generation, cancellation.Token)).Token == RotationToken,
                    "Late cancellation discarded an accepted backup preparation.");
            else
                Check((await store.CreateBackupAsync(Generation, RotationToken, cancellation.Token)).Token == RestoreToken,
                    "Late cancellation discarded a confirmed backup rotation.");
            Check(accepted.ResultDestroys == 1, "Late cancellation leaked a backup preparation/rotation result.");
        }

        var race = new FakeNative { Reply = PreparationReply(false), ReplyPayload = null };
        using (var entered = new ManualResetEventSlim())
        using (var release = new ManualResetEventSlim())
        {
            race.OnDispatch = () => { entered.Set(); if (!release.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException(); };
            var store = await ConfigurationStore.OpenAsync(configuration, race);
            var accepted = store.PrepareBackupAsync(Generation);
            Check(entered.Wait(TimeSpan.FromSeconds(10)), "Backup preparation did not start.");
            var queued = store.CreateBackupAsync(Generation, RotationToken);
            store.Dispose();
            Check(race.StoreDestroys == 0, "Disposal freed a running backup preparation owner.");
            release.Set();
            Check((await accepted).Token == RotationToken, "Disposal discarded an accepted backup preparation.");
            await RejectAsync(queued, typeof(ObjectDisposedException));
            await RejectAsync(store.PrepareBackupAsync(Generation), typeof(ObjectDisposedException));
            Check(race.StoreDestroys == 1 && race.ResultDestroys == 1 && race.Dispatches == 1,
                "Disposed/queued backup operations leaked owners or reached native dispatch.");
        }
    }

    internal static async Task RunNativeAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "configuration-backup-rotation-" + Guid.NewGuid());
        Directory.CreateDirectory(directory);
        try
        {
            using var store = await ConfigurationStore.OpenAsync(new Configuration(directory, "backup.rotation.test"));
            var saved = await store.SaveAsync(null, new byte[] { 0, 255, 128, 42 });
            var absent = await store.PrepareBackupAsync(saved.Generation!);
            Check(absent.Backup is null, "Preparation fabricated a backup for an initialized store without one.");
            var backup = await store.CreateBackupAsync(saved.Generation!, absent.Token);

            var review = await store.PrepareBackupAsync(saved.Generation!);
            Check(review.Backup?.Token == backup.Token && review.Token != backup.Token
                && review.Backup.Snapshot.Payload!.SequenceEqual(saved.Payload!),
                "Native preparation did not separate inspected backup bytes/restoration identity from rotation authorization.");
            await FailureAsync(store.CreateBackupAsync(saved.Generation!, backup.Token), ErrorCode.BackupChanged);
            var retained = await store.ReadBackupAsync();
            Check(retained.Token == backup.Token && retained.Snapshot.Payload!.SequenceEqual(saved.Payload!)
                && (await store.LoadAsync()).Generation == saved.Generation,
                "A rejected rotation changed the persisted backup or primary generation.");
        }
        finally { Directory.Delete(directory, true); }
        Console.WriteLine("Native backup round trip and typed rotation rejection passed.");
    }
}
