using System.Text.Json;
using NaviApps.ConfigurationStorage;
using static TestAssertions;

static class SnapshotCandidateTests
{
    private const string CandidateId = "opaque-candidate";
    private const string PageReply = """
        {"candidates":[{"id":"Z","source":"generation","isSelected":true},{"id":"a","source":"generation","isSelected":false},{"id":"opaque-candidate","source":"generation","isSelected":null}],"nextCursor":"opaque-candidate"}
        """;

    private static string ReadReply(string snapshot = FakeNative.Initialized, string selection = "false",
        string modifiedAt = "-1.25", string id = "\"opaque-candidate\"")
        => "{\"id\":" + id + ",\"snapshot\":" + snapshot + ",\"modifiedAt\":" + modifiedAt
            + ",\"source\":\"backup\",\"isSelected\":" + selection + "}";

    internal static async Task RunManagedAsync()
    {
        var configuration = new Configuration(Path.Combine(Path.GetTempPath(), "candidate-fake-" + Guid.NewGuid()), "candidate.test", 3);
        var fake = new FakeNative { Reply = PageReply, ReplyPayload = null };
        using (var store = await ConfigurationStore.OpenAsync(configuration, fake))
        {
            var page = await store.ListSnapshotCandidatesAsync();
            using (var command = JsonDocument.Parse(fake.CommandJson!))
                Check(command.RootElement.EnumerateObject().Count() == 4
                    && command.RootElement.GetProperty("command").GetString() == "listSnapshotCandidates"
                    && command.RootElement.GetProperty("limit").GetInt32() == 64
                    && command.RootElement.GetProperty("after").ValueKind == JsonValueKind.Null
                    && command.RootElement.GetProperty("source").ValueKind == JsonValueKind.Null,
                    "Candidate listing changed default limit, cursor or command framing.");
            Check(page.Candidates.Count == 3 && page.Candidates[0] == new SnapshotCandidateReference("Z", SnapshotCandidateSource.Generation, true)
                && page.Candidates[1].IsSelected == false && page.Candidates[2].IsSelected is null
                && page.NextCursor == CandidateId, "Candidate metadata lost ordinal IDs or nullable selection.");
            var equivalent = new SnapshotCandidatePage(page.Candidates, page.NextCursor);
            Check(page == equivalent && new HashSet<SnapshotCandidatePage> { page }.Contains(equivalent),
                "Owned candidate page comparison or hashing used reference identity.");
            var changedSelection = page.Candidates.Select(candidate => candidate with { IsSelected = false });
            Check(page != new SnapshotCandidatePage(changedSelection, page.NextCursor)
                && page != new SnapshotCandidatePage(page.Candidates, null),
                "Page equality ignored candidate selection metadata or cursor.");
            Check(((IList<SnapshotCandidateReference>)page.Candidates).IsReadOnly, "Page exposes a mutable candidate collection.");
            var references = new[] { new SnapshotCandidateReference("original", SnapshotCandidateSource.Generation, null) };
            var copiedPage = new SnapshotCandidatePage(references, null);
            references[0] = new("changed", SnapshotCandidateSource.Backup, true);
            Check(copiedPage.Candidates[0].Id == "original", "Page retained a caller-owned array.");

            await store.ListSnapshotCandidatesAsync(1, CandidateId, SnapshotCandidateSource.Backup);
            using (var command = JsonDocument.Parse(fake.CommandJson!))
                Check(command.RootElement.GetProperty("limit").GetInt32() == 1
                    && command.RootElement.GetProperty("after").GetString() == CandidateId
                    && command.RootElement.GetProperty("source").GetString() == "backup",
                    "Facade parsed or changed an opaque candidate cursor.");
            fake.Reply = """{"candidates":[],"nextCursor":null}""";
            Check((await store.ListSnapshotCandidatesAsync()).Candidates.Count == 0
                && page.Candidates.Count == 3 && page.NextCursor == CandidateId, "Pages borrowed a later native result.");

            fake.ReplyPayload = [1, 2, 3];
            foreach (var selection in new[] { "true", "false", "null" })
                foreach (var seconds in new[] { 1700000000.125, 0, -1.25, -11644473600.0, -1e300, 1e300 })
                {
                    fake.Reply = ReadReply(selection: selection, modifiedAt: JsonSerializer.Serialize(seconds));
                    var candidate = await store.ReadSnapshotCandidateAsync(CandidateId);
                    Check(candidate.Id == CandidateId && candidate.Source == SnapshotCandidateSource.Backup && candidate.Snapshot.Generation == FakeNative.Generation
                        && candidate.Snapshot.Payload!.SequenceEqual(new byte[] { 1, 2, 3 }) && candidate.ModifiedAt == seconds
                        && candidate.IsSelected == (selection == "null" ? (bool?)null : selection == "true"),
                        "Candidate read lost opaque identity, signed finite time, payload or nullable selection.");
                    candidate.Snapshot.Payload![0] = 99;
                    Check(candidate.Snapshot.Payload![0] == 1, "Candidate payload getter exposed shared mutable bytes.");
                }
            using (var command = JsonDocument.Parse(fake.CommandJson!))
                Check(command.RootElement.EnumerateObject().Count() == 2
                    && command.RootElement.GetProperty("command").GetString() == "readSnapshotCandidate"
                    && command.RootElement.GetProperty("id").GetString() == CandidateId,
                    "Facade parsed or changed an opaque candidate read ID.");
            fake.Reply = ReadReply();
            fake.ReplyPayload = [];
            Check((await store.ReadSnapshotCandidateAsync(CandidateId)).Snapshot.Payload is { Length: 0 },
                "Initialized empty candidate payload became uninitialized.");

            await InvalidPagesAsync(store, fake);
            await InvalidReadsAsync(store, fake);
            foreach (var (wire, code) in new[] {
                ("snapshotCandidateMissing", ErrorCode.SnapshotCandidateMissing),
                ("rawRecoveryLimitExceeded", ErrorCode.RawRecoveryLimitExceeded) })
            {
                fake.ReplyCode = 1;
                fake.Reply = JsonSerializer.Serialize(new { code = wire, recoveryDirectory = configuration.Directory });
                await FailureAsync(store.ReadSnapshotCandidateAsync(CandidateId), code);
                await FailureAsync(store.ListSnapshotCandidatesAsync(), code);
            }
            fake.ReplyCode = 0;
            fake.OnDispatch = () =>
            {
                using var command = JsonDocument.Parse(fake.CommandJson!);
                var name = command.RootElement.GetProperty("command").GetString();
                fake.Reply = name switch
                {
                    "listSnapshotCandidates" => PageReply,
                    "readSnapshotCandidate" => ReadReply(),
                    _ => FakeNative.Initialized
                };
                fake.ReplyPayload = name == "listSnapshotCandidates" ? null : [1, 2, 3];
            };
            await Task.WhenAll(Enumerable.Range(0, 8).SelectMany(_ => new Task[] {
                store.LoadAsync(), store.ListSnapshotCandidatesAsync(), store.ReadSnapshotCandidateAsync(CandidateId) }));
            Check(fake.MaximumActive == 1, "Candidate operations bypassed the existing store serialization gate.");
        }
        Check(fake.StoreDestroys == 1 && fake.ResultDestroys == fake.Dispatches, "Candidate decoding leaked native owners.");
        await LifetimeAsync(configuration);
    }

    private static async Task InvalidPagesAsync(ConfigurationStore store, FakeNative fake)
    {
        fake.Reply = PageReply;
        await RejectAsync(store.ListSnapshotCandidatesAsync(), typeof(InvalidDataException));
        fake.ReplyPayload = null;
        foreach (var reply in new[] {
            "null", "[]", "{}", """{"candidates":[]}""", """{"nextCursor":null}""",
            """{"candidates":null,"nextCursor":null}""", """{"candidates":{},"nextCursor":null}""",
            """{"candidates":[null],"nextCursor":null}""", """{"candidates":[{}],"nextCursor":null}""",
            """{"candidates":[{"id":"a"}],"nextCursor":null}""",
            """{"candidates":[{"source":"generation","isSelected":true}],"nextCursor":null}""",
            """{"candidates":[{"id":"","source":"generation","isSelected":false}],"nextCursor":null}""",
            """{"candidates":[{"id":null,"source":"generation","isSelected":false}],"nextCursor":null}""",
            """{"candidates":[{"id":1,"source":"generation","isSelected":false}],"nextCursor":null}""",
            """{"candidates":[{"id":"a","source":"generation","isSelected":"false"}],"nextCursor":null}""",
            """{"candidates":[{"id":"a","source":"generation","isSelected":0}],"nextCursor":null}""",
            """{"candidates":[{"id":"a","source":"generation","isSelected":false},{"id":"a","source":"generation","isSelected":null}],"nextCursor":null}""",
            """{"candidates":[{"id":"b","source":"generation","isSelected":false},{"id":"a","source":"generation","isSelected":true}],"nextCursor":null}""",
            """{"candidates":[],"nextCursor":"a"}""", """{"candidates":[],"nextCursor":""}""",
            PageReply.Replace("\"nextCursor\":\"opaque-candidate\"", "\"nextCursor\":\"a\""),
            PageReply.Replace("\"nextCursor\":\"opaque-candidate\"", "\"nextCursor\":\"\""),
            PageReply.Replace("\"nextCursor\":\"opaque-candidate\"", "\"nextCursor\":false") })
        {
            fake.Reply = reply;
            await RejectAsync(store.ListSnapshotCandidatesAsync(), typeof(InvalidDataException));
        }
        foreach (var count in new[] { 256, 257 })
        {
            var values = Enumerable.Range(0, count).Select(index => new { id = $"opaque-{index:D3}", source = "generation", isSelected = (bool?)null }).ToArray();
            fake.Reply = JsonSerializer.Serialize(new { candidates = values, nextCursor = values[^1].id });
            if (count == 256)
                Check((await store.ListSnapshotCandidatesAsync(256)).Candidates.Count == 256, "Maximum-size candidate page was rejected.");
            else
                await RejectAsync(store.ListSnapshotCandidatesAsync(256), typeof(InvalidDataException));
        }
    }

    private static async Task InvalidReadsAsync(ConfigurationStore store, FakeNative fake)
    {
        fake.ReplyPayload = [1, 2, 3];
        foreach (var reply in new[] {
            "null", "[]", "{}",
            ReadReply().Replace("\"id\":\"opaque-candidate\",", ""),
            ReadReply().Replace("\"snapshot\":" + FakeNative.Initialized + ",", ""),
            ReadReply().Replace("\"modifiedAt\":-1.25,", ""),
            ReadReply().Replace(",\"isSelected\":false", ""),
            ReadReply().Replace(",\"source\":\"backup\"", ""),
            ReadReply().Replace("\"backup\"", "\"unknown\""),
            ReadReply(id: "null"), ReadReply(id: "\"\""), ReadReply(id: "false"),
            ReadReply(selection: "0"), ReadReply(selection: "\"true\""),
            ReadReply(modifiedAt: "null"), ReadReply(modifiedAt: "\"-1.25\""),
            ReadReply(modifiedAt: "1e999"), ReadReply(modifiedAt: "-1e999"),
            ReadReply(snapshot: "null"), ReadReply(snapshot: "{}"), ReadReply(snapshot: "[]"),
            ReadReply(snapshot: """{"generation":null}"""),
            ReadReply(snapshot: FakeNative.Initialized.Replace(FakeNative.Generation, "")) })
        {
            fake.Reply = reply;
            await RejectAsync(store.ReadSnapshotCandidateAsync(CandidateId), typeof(InvalidDataException));
        }
        fake.Reply = ReadReply();
        foreach (var payload in new byte[]?[] { null, [1, 2, 3, 4] })
        {
            fake.ReplyPayload = payload;
            await RejectAsync(store.ReadSnapshotCandidateAsync(CandidateId), typeof(InvalidDataException));
        }
    }

    private static async Task LifetimeAsync(Configuration configuration)
    {
        foreach (var read in new[] { false, true })
        {
            var fake = new FakeNative { Reply = read ? ReadReply() : PageReply, ReplyPayload = read ? [1, 2, 3] : null };
            using var store = await ConfigurationStore.OpenAsync(configuration, fake);
            using var source = new CancellationTokenSource();
            source.Cancel();
            await RejectAsync(store.ListSnapshotCandidatesAsync(cancellationToken: source.Token), typeof(OperationCanceledException));
            await RejectAsync(store.ReadSnapshotCandidateAsync(CandidateId, source.Token), typeof(OperationCanceledException));
            Check(fake.Dispatches == 0, "Pre-cancelled candidate work reached native dispatch.");

            using var started = new ManualResetEventSlim();
            using var finish = new ManualResetEventSlim();
            using var acceptedCancellation = new CancellationTokenSource();
            using var queuedCancellation = new CancellationTokenSource();
            fake.OnDispatch = () => { started.Set(); if (!finish.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException(); };
            Task accepted = read ? store.ReadSnapshotCandidateAsync(CandidateId, acceptedCancellation.Token)
                : store.ListSnapshotCandidatesAsync(cancellationToken: acceptedCancellation.Token);
            Task? disposedList = null;
            Task? disposedRead = null;
            try
            {
                Check(started.Wait(TimeSpan.FromSeconds(10)), "Candidate lifetime race did not start.");
                var queuedList = store.ListSnapshotCandidatesAsync(cancellationToken: queuedCancellation.Token);
                var queuedRead = store.ReadSnapshotCandidateAsync(CandidateId, queuedCancellation.Token);
                queuedCancellation.Cancel();
                await RejectAsync(queuedList, typeof(OperationCanceledException));
                await RejectAsync(queuedRead, typeof(OperationCanceledException));
                disposedList = store.ListSnapshotCandidatesAsync();
                disposedRead = store.ReadSnapshotCandidateAsync(CandidateId);
                acceptedCancellation.Cancel();
                store.Dispose();
                Check(fake.StoreDestroys == 0, "Disposal freed an accepted candidate operation's native owner.");
            }
            finally { finish.Set(); }
            await accepted;
            await RejectAsync(disposedList!, typeof(ObjectDisposedException));
            await RejectAsync(disposedRead!, typeof(ObjectDisposedException));
            await RejectAsync(store.ListSnapshotCandidatesAsync(), typeof(ObjectDisposedException));
            await RejectAsync(store.ReadSnapshotCandidateAsync(CandidateId), typeof(ObjectDisposedException));
            Check(fake.StoreDestroys == 1 && fake.Dispatches == 1 && fake.ResultDestroys == 1,
                "Candidate cancellation/disposal violated result delivery or native ownership.");
        }
        using var lateCancellation = new CancellationTokenSource();
        var rejection = new FakeNative { ReplyCode = 1, Reply = """{"code":"rawRecoveryLimitExceeded"}""", OnDispatch = lateCancellation.Cancel };
        using var rejectedStore = await ConfigurationStore.OpenAsync(configuration, rejection);
        await FailureAsync(rejectedStore.ReadSnapshotCandidateAsync(CandidateId, lateCancellation.Token), ErrorCode.RawRecoveryLimitExceeded);
        Check(rejection.ResultDestroys == 1, "Late cancellation discarded a candidate failure result.");
    }

    internal static async Task RunNativeAsync()
    {
        var directory = Path.Combine(Path.GetTempPath(), "configuration-candidates-" + Guid.NewGuid());
        try
        {
            using var store = await ConfigurationStore.OpenAsync(new(directory, "candidate.test", 1024));
            var payload = new byte[] { 128, 0, 66, 254 };
            var original = await store.SaveAsync(null, payload);
            var replacement = new byte[] { 7 };
            var prepareReplacement = await store.PrepareRestoreAsync(replacement);
            await store.RestoreSnapshotAsync(replacement, prepareReplacement);

            var first = await store.ListSnapshotCandidatesAsync(1, source: SnapshotCandidateSource.Generation);
            Check(first.NextCursor is not null, "Native candidate cursor was lost.");
            var second = await store.ListSnapshotCandidatesAsync(1, first.NextCursor, SnapshotCandidateSource.Generation);
            var reference = first.Candidates.Concat(second.Candidates).Single(candidate => candidate.IsSelected == false);
            var held = await store.ReadSnapshotCandidateAsync(reference.Id);
            Check(held.Id == reference.Id && held.Source == SnapshotCandidateSource.Generation
                && held.Snapshot.Generation == original.Generation && held.Snapshot.Payload!.SequenceEqual(payload),
                "Native retained candidate lost its identity or bytes.");
            held.Snapshot.Payload![0] ^= 1;
            var restoredBytes = held.Snapshot.Payload!;
            var token = await store.PrepareRestoreAsync(restoredBytes);
            var restored = await store.RestoreSnapshotAsync(restoredBytes, token);
            Check(restored.Payload!.SequenceEqual(payload) && held.Snapshot.Payload!.SequenceEqual(payload),
                "Native candidate payload ownership or transfer into restore failed.");
            await FailureAsync(store.ListSnapshotCandidatesAsync(-1), ErrorCode.InvalidConfiguration);
        }
        finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
        Console.WriteLine("Native candidate cursor, payload ownership and explicit restore passed.");
    }
}
