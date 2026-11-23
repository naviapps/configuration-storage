using System.Text.Json;
using NaviApps.ConfigurationStorage;
using static TestAssertions;

static class OpenExistingTests
{
    internal static async Task RunManagedAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "configuration-open-existing-fake");
        var configuration = new Configuration(directory, "existing.test", 1024);
        var native = new FakeNative();
        using (var store = await ConfigurationStore.OpenExistingAsync(configuration, native))
        {
            Check((await store.LoadAsync()).Generation == FakeNative.Generation,
                "The existing-only handle did not support regular store operations.");
            using var json = JsonDocument.Parse(native.ConfigurationJson!);
            Check(json.RootElement.GetProperty("directory").GetString() == directory
                && json.RootElement.GetProperty("owner").GetString() == "existing.test"
                && json.RootElement.GetProperty("maximumPayloadBytes").GetInt32() == 1024,
                "Existing-only open changed the configuration framing.");
        }
        Check(native.Opens == 0 && native.ExistingOpens == 1 && native.StoreDestroys == 1 && native.ResultDestroys == 1,
            "Existing-only open used the creating entry point or leaked native owners.");

        var malformedPath = configuration with { Directory = directory + '\uD800' };
        var malformed = new FakeNative();
        await FailureAsync(ConfigurationStore.OpenExistingAsync(malformedPath, malformed), ErrorCode.InvalidConfiguration);
        await FailureAsync(ConfigurationStore.OpenAsync(malformedPath, malformed), ErrorCode.InvalidConfiguration);
        Check(malformed.AbiCalls == 0, "An unrepresentable path reached a native store entry point.");

        foreach (var (reply, code) in new[]
        {
            ("""{"code":"storeMissing"}""", ErrorCode.StoreMissing),
            ("""{"code":"ownerMismatch"}""", ErrorCode.OwnerMismatch),
            ("""{"code":"invalidStoredData"}""", ErrorCode.InvalidStoredData)
        })
        {
            using var cancellation = new CancellationTokenSource();
            var rejected = new FakeNative { OpenFailure = reply, OnOpen = cancellation.Cancel };
            await FailureAsync(ConfigurationStore.OpenExistingAsync(configuration, rejected, cancellation.Token), code);
            Check(rejected.Opens == 0 && rejected.ExistingOpens == 1 && rejected.ResultDestroys == 1 && rejected.StoreDestroys == 0,
                "An existing-only failure was retried through creation, discarded on late cancellation, or leaked its result.");
        }

        var old = new FakeNative { Version = PInvokeStorageNative.SupportedAbiVersion - 1 };
        await FailureAsync(ConfigurationStore.OpenExistingAsync(configuration, old), ErrorCode.UnsupportedAbiVersion);
        Check(old.Opens == 0 && old.ExistingOpens == 0 && old.AbiCalls == 1,
            "Existing-only open attempted to dispatch against the previous native ABI.");
    }

    internal static async Task RunNativeAsync()
    {
        var fixture = Directory.CreateTempSubdirectory("configuration-open-existing-");
        try
        {
            var parent = Path.Combine(fixture.FullName, "missing-parent");
            var configuration = new Configuration(Path.Combine(parent, "store"), "existing.test", 1024);
            await FailureAsync(ConfigurationStore.OpenExistingAsync(configuration), ErrorCode.StoreMissing);
            Check(!Directory.Exists(parent), "Existing-only open created missing ancestors.");

            (await ConfigurationStore.OpenAsync(configuration)).Dispose();
            using var existing = await ConfigurationStore.OpenExistingAsync(configuration);
            var initial = await existing.LoadAsync();
            Check(initial.Generation is null && initial.Payload is null, "An owned empty root did not load as uninitialized.");
            var payload = new byte[] { 0, 255, 42 };
            var saved = await existing.SaveAsync(null, payload);
            using var reopened = await ConfigurationStore.OpenExistingAsync(configuration);
            var loaded = await reopened.LoadAsync();
            Check(loaded.Generation == saved.Generation && loaded.Payload!.SequenceEqual(payload),
                "Existing-only open did not preserve a managed snapshot.");
            await FailureAsync(ConfigurationStore.OpenExistingAsync(configuration with { Owner = "foreign.test" }), ErrorCode.OwnerMismatch);

            var selection = Path.Combine(configuration.Directory, "active-generation");
            await File.WriteAllBytesAsync(selection, new byte[] { 255, 0, 47 });
            using var damaged = await ConfigurationStore.OpenExistingAsync(configuration);
            await FailureAsync(damaged.LoadAsync(), ErrorCode.InvalidStoredData);
        }
        finally { fixture.Delete(recursive: true); }
        Console.WriteLine("Native existing-only admission and non-creation checks passed.");
    }
}
