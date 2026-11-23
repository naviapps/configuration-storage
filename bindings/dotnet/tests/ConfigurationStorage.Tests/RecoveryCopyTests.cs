using System.Text.Json;
using NaviApps.ConfigurationStorage;
using static TestAssertions;

static class RecoveryCopyTests
{
    private const int Limit = ConfigurationRecoveryCopy.DefaultMaximumPayloadBytes;
    private static readonly string Digest = new('a', 64);
    private static readonly string Reply = JsonSerializer.Serialize(new { payload = "AP8B", digest = Digest, modifiedAt = 1700000000.125 });

    internal static async Task RunManagedAsync()
    {
        var path = Path.Combine(Path.GetTempPath(), "recovery-copy-" + Guid.NewGuid());
        var fake = new FakeNative { Reply = Reply };
        var copy = await ConfigurationRecoveryCopy.ReadAsync(path, Limit, fake);
        Check(copy.Payload.SequenceEqual(new byte[] { 0, 255, 1 }) && copy.Digest == Digest
            && copy.ModifiedAt == 1700000000.125, "Recovery-copy bytes, digest or fractional time changed.");
        copy.Payload[0] = 99;
        Check(copy.Payload[0] == 0, "Recovery-copy payload ownership escaped.");
        Check(copy == new ConfigurationRecoveryCopy([0, 255, 1], Digest, copy.ModifiedAt)
            && copy != new ConfigurationRecoveryCopy([0, 255, 2], Digest, copy.ModifiedAt)
            && copy != new ConfigurationRecoveryCopy([0, 255, 1], new string('b', 64), copy.ModifiedAt)
            && copy != new ConfigurationRecoveryCopy([0, 255, 1], Digest, copy.ModifiedAt + 1),
            "Recovery-copy equality ignored payload, digest or date.");
        var signedZero = new ConfigurationRecoveryCopy([], Digest, -0.0);
        var zero = new ConfigurationRecoveryCopy([], Digest, 0.0);
        Check(signedZero == zero && new HashSet<ConfigurationRecoveryCopy> { signedZero }.Contains(zero),
            "Equal signed-zero recovery dates did not share hash semantics.");
        using (var command = JsonDocument.Parse(fake.RecoveryCopyCommandJson!))
        {
            var root = command.RootElement;
            Check(root.EnumerateObject().Count() == 3 && root.GetProperty("command").GetString() == "read"
                && root.GetProperty("path").GetString() == path && root.GetProperty("maximumPayloadBytes").GetInt32() == Limit,
                "Standalone read acquired store or owner configuration.");
        }
        fake.Reply = "null";
        using (var entered = new ManualResetEventSlim())
        using (var release = new ManualResetEventSlim())
        {
            fake.OnRecoveryCopyDispatch = () => { entered.Set(); if (!release.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException(); };
            var input = new byte[] { 0, 255, 1 };
            var write = ConfigurationRecoveryCopy.WriteAsync(path, input, Limit, fake);
            input[0] = 99;
            try { Check(entered.Wait(TimeSpan.FromSeconds(10)), "Recovery write did not start."); }
            finally { release.Set(); }
            await write;
            using var command = JsonDocument.Parse(fake.RecoveryCopyCommandJson!);
            var root = command.RootElement;
            Check(root.EnumerateObject().Count() == 4 && root.GetProperty("command").GetString() == "write"
                && root.GetProperty("path").GetString() == path && root.GetProperty("maximumPayloadBytes").GetInt32() == Limit
                && root.GetProperty("payload").GetString() == "AP8B", "Recovery write did not capture caller bytes or wire options.");
        }
        Check(fake.Opens == 0 && fake.Dispatches == 0 && fake.StoreDestroys == 0
            && fake.AbiCalls == 2 && fake.ResultDestroys == 2, "Standalone recovery used or leaked store/result ownership.");

        foreach (var version in FakeNative.IncompatibleVersions)
        {
            var incompatible = new FakeNative { Version = version };
            await FailureAsync(ConfigurationRecoveryCopy.ReadAsync(path, Limit, incompatible), ErrorCode.UnsupportedAbiVersion);
            await FailureAsync(ConfigurationRecoveryCopy.WriteAsync(path, new byte[] { 1 }, Limit, incompatible), ErrorCode.UnsupportedAbiVersion);
            Check(incompatible.RecoveryCopyDispatches == 0 && incompatible.Opens == 0 && incompatible.ResultDestroys == 0,
                "Incompatible ABI reached the new recovery symbol.");
        }
        using (var source = new CancellationTokenSource())
        {
            source.Cancel();
            var cancelled = new FakeNative();
            await RejectAsync(ConfigurationRecoveryCopy.ReadAsync(path, Limit, cancelled, source.Token), typeof(OperationCanceledException));
            await RejectAsync(ConfigurationRecoveryCopy.WriteAsync(path, new byte[] { 1 }, Limit, cancelled, source.Token), typeof(OperationCanceledException));
            Check(cancelled.AbiCalls == 0 && cancelled.RecoveryCopyDispatches == 0, "Pre-cancelled recovery reached native code.");
        }
        foreach (var write in new[] { false, true })
        {
            using var source = new CancellationTokenSource();
            var beforeDispatch = new FakeNative { OnAbi = source.Cancel };
            await RejectAsync(write
                ? ConfigurationRecoveryCopy.WriteAsync(path, new byte[] { 1 }, Limit, beforeDispatch, source.Token)
                : ConfigurationRecoveryCopy.ReadAsync(path, Limit, beforeDispatch, source.Token), typeof(OperationCanceledException));
            Check(beforeDispatch.RecoveryCopyDispatches == 0, "Cancellation during ABI validation reached recovery dispatch.");

            using var late = new CancellationTokenSource();
            var accepted = new FakeNative { Reply = write ? "null" : Reply, OnRecoveryCopyDispatch = late.Cancel };
            if (write) await ConfigurationRecoveryCopy.WriteAsync(path, new byte[] { 1 }, Limit, accepted, late.Token);
            else await ConfigurationRecoveryCopy.ReadAsync(path, Limit, accepted, late.Token);
            Check(accepted.ResultDestroys == 1, "Late cancellation discarded an owned recovery result.");

            using var failed = new CancellationTokenSource();
            var acceptedFailure = new FakeNative { ReplyCode = 1, Reply = "{\"code\":\"ioFailure\"}", OnRecoveryCopyDispatch = failed.Cancel };
            await FailureAsync(write
                ? ConfigurationRecoveryCopy.WriteAsync(path, new byte[] { 1 }, Limit, acceptedFailure, failed.Token)
                : ConfigurationRecoveryCopy.ReadAsync(path, Limit, acceptedFailure, failed.Token), ErrorCode.IoFailure);
            Check(acceptedFailure.ResultDestroys == 1, "Late cancellation lost a recovery failure result.");
        }

        var invalid = new FakeNative();
        foreach (var invalidPath in new[] { "", "relative.copy", path + '\0', path + '\uD800' })
        {
            await FailureAsync(ConfigurationRecoveryCopy.ReadAsync(invalidPath, Limit, invalid), ErrorCode.InvalidConfiguration);
            await FailureAsync(ConfigurationRecoveryCopy.WriteAsync(invalidPath, new byte[] { 1 }, Limit, invalid), ErrorCode.InvalidConfiguration);
        }
        foreach (var maximum in new[] { 0, -1, ConfigurationRecoveryCopy.MaximumSupportedPayloadBytes + 1 })
        {
            await FailureAsync(ConfigurationRecoveryCopy.ReadAsync(path, maximum, invalid), ErrorCode.InvalidConfiguration);
            await FailureAsync(ConfigurationRecoveryCopy.WriteAsync(path, new byte[] { 1 }, maximum, invalid), ErrorCode.InvalidConfiguration);
        }
        await FailureAsync(ConfigurationRecoveryCopy.WriteAsync(path, new byte[] { 1, 2 }, 1, invalid), ErrorCode.InvalidConfiguration);
        Check(invalid.AbiCalls == 0, "Invalid recovery configuration or oversized input reached native code.");
        invalid.Reply = Reply.Replace("AP8B", "");
        Check((await ConfigurationRecoveryCopy.ReadAsync(path, ConfigurationRecoveryCopy.MaximumSupportedPayloadBytes, invalid)).Payload.Length == 0,
            "The maximum supported recovery limit or an empty payload was rejected.");

        var malformed = new FakeNative();
        foreach (var reply in new[] { "{", "null", "[]", "{}", Reply.Replace("AP8B", "!"),
            Reply.Replace("\"AP8B\"", "null"), Reply.Replace(Digest, "f"), Reply.Replace(Digest, new string('g', 64)),
            Reply.Replace(Digest, Digest.ToUpperInvariant()),
            Reply.Replace("1700000000.125", "-1e999"), Reply.Replace("1700000000.125", "1e999"),
            Reply.Replace("1700000000.125", "null") })
        {
            malformed.Reply = reply;
            await RejectAsync(ConfigurationRecoveryCopy.ReadAsync(path, Limit, malformed), typeof(InvalidDataException));
        }
        malformed.Reply = Reply;
        await RejectAsync(ConfigurationRecoveryCopy.ReadAsync(path, 1, malformed), typeof(InvalidDataException));
        foreach (var reply in new[] { "{}", "false", "\"metadata\"", "[]" })
        {
            malformed.Reply = reply;
            await RejectAsync(ConfigurationRecoveryCopy.WriteAsync(path, ReadOnlyMemory<byte>.Empty, Limit, malformed), typeof(InvalidDataException));
        }
        malformed.ReplyCode = 1;
        foreach (var code in new[] { ErrorCode.InvalidRecoveryCopy, ErrorCode.UnsupportedRecoveryVersion, ErrorCode.RestoreChanged })
        {
            var text = code.ToString();
            malformed.Reply = JsonSerializer.Serialize(new { code = char.ToLowerInvariant(text[0]) + text[1..] });
            await FailureAsync(ConfigurationRecoveryCopy.ReadAsync(path, Limit, malformed), code);
        }
        malformed.Reply = "null";
        await RejectAsync(ConfigurationRecoveryCopy.WriteAsync(path, ReadOnlyMemory<byte>.Empty, Limit, malformed), typeof(InvalidDataException));
        malformed.NullDispatch = true;
        await RejectAsync(ConfigurationRecoveryCopy.ReadAsync(path, Limit, malformed), typeof(InvalidDataException));
        Check(malformed.ResultDestroys == malformed.RecoveryCopyDispatches - 1, "Invalid recovery replies leaked or double-freed owners.");
    }

    internal static async Task RunNativeAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "configuration-recovery-copy-" + Guid.NewGuid());
        Directory.CreateDirectory(directory);
        try
        {
            var path = Path.Combine(directory, "portable.copy");
            var payload = Enumerable.Range(0, 256).Select(value => (byte)value).ToArray();
            await ConfigurationRecoveryCopy.WriteAsync(path, payload);
            var file = await File.ReadAllBytesAsync(path);
            var copy = await ConfigurationRecoveryCopy.ReadAsync(path);
            Check(copy.Payload.SequenceEqual(payload) && copy.Digest.Length == 64 && copy.Digest.All(char.IsAsciiHexDigit)
                && double.IsFinite(copy.ModifiedAt) && copy.ModifiedAt >= 0 && file.Length > payload.Length,
                "Standalone native file did not retain framed binary bytes and metadata.");
            await FailureAsync(ConfigurationRecoveryCopy.ReadAsync(path, payload.Length - 1), ErrorCode.InvalidRecoveryCopy);
            file[^1] ^= 1;
            await File.WriteAllBytesAsync(path, file);
            await FailureAsync(ConfigurationRecoveryCopy.ReadAsync(path), ErrorCode.InvalidRecoveryCopy);
            await ConfigurationRecoveryCopy.WriteAsync(path, ReadOnlyMemory<byte>.Empty);
            Check((await ConfigurationRecoveryCopy.ReadAsync(path)).Payload.Length == 0, "Empty managed payload did not round-trip.");

            await DefaultPayloadLimitAsync(Path.Combine(directory, "boundary.copy"));
        }
        finally { Directory.Delete(directory, true); }
        Console.WriteLine("Native standalone recovery-copy binary roundtrip, default byte limit and typed integrity failures passed.");
    }

    private static async Task DefaultPayloadLimitAsync(string path)
    {
        const int boundaryBytes = 16 * 1024 * 1024;
        var input = new byte[boundaryBytes + 1];
        Array.Fill(input, (byte)255);
        var boundary = input.AsMemory(0, boundaryBytes);
        await ConfigurationRecoveryCopy.WriteAsync(path, boundary);
        var copy = await ConfigurationRecoveryCopy.ReadAsync(path);
        Check(copy.Payload.AsSpan().SequenceEqual(boundary.Span), "The default recovery limit did not retain exactly 16 MiB.");
        await FailureAsync(ConfigurationRecoveryCopy.WriteAsync(path, input), ErrorCode.InvalidConfiguration);
    }
}
