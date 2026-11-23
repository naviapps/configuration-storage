using NaviApps.ConfigurationStorage;
var directory = Path.Combine(Path.GetTempPath(), "configuration-storage-consumer-" + Guid.NewGuid());
try
{
    var storeDirectory = Path.Combine(directory, "store");
    using var store = ConfigurationStore.CreateLazy(new Configuration(storeDirectory, "package-test"));
    using var existing = ConfigurationStore.CreateLazyExisting(new Configuration(storeDirectory, "package-test"));
    if (Directory.Exists(storeDirectory)) throw new Exception("Constructing a facade opened a store.");
    try { await existing.LoadAsync(); throw new Exception("Existing-only admission succeeded for a missing store."); }
    catch (StorageException error) when (error.Failure.Code == ErrorCode.StoreMissing) { }
    if (Directory.Exists(storeDirectory)) throw new Exception("Existing-only admission created a store.");
    if ((await store.LoadAsync()).IsInitialized) throw new Exception("Expected an empty disposable store.");
    var input = new byte[] { 0, 255, 42 };
    var writing = store.SaveAsync(null, input);
    input[0] = 99;
    var saved = await writing;
    if (await existing.LoadAsync() != saved) throw new Exception("The existing store did not reopen after creation.");
    if (saved.Payload![0] != 0) throw new Exception("Input bytes were borrowed.");
    saved.Payload![0] = 99;
    if (saved.Payload![0] != 0) throw new Exception("Snapshot bytes were borrowed.");
    var preparation = await store.PrepareBackupAsync(saved.Generation!);
    if (preparation.Backup is not null) throw new Exception("Expected no prior checkpoint in this disposable store.");
    var backup = await store.CreateBackupAsync(saved.Generation!, preparation.Token);
    await store.SaveAsync(saved.Generation, ReadOnlyMemory<byte>.Empty);
    try { await store.RestoreBackupAsync(backup.Token); throw new Exception("Stale validation token overwrote a newer write."); }
    catch (StorageException error) when (error.Failure.Code == ErrorCode.BackupChanged) { }
    var marker = Path.Combine(storeDirectory, "active-backup");
    var backups = Path.Combine(storeDirectory, "Backups");
    var selected = Path.Combine(backups, File.ReadAllText(marker) + ".sqlite");
    File.Move(selected, Path.Combine(backups, "snapshot.sqlite"));
    File.Delete(marker);
    var metadata = Path.Combine(backups, "desktop.ini");
    var metadataBytes = new byte[] { 4, 5, 6 };
    File.WriteAllBytes(metadata, metadataBytes);
    var validated = await store.ReadBackupAsync();
    if (!validated.Snapshot.Payload!.SequenceEqual(saved.Payload!)) throw new Exception("Backup semantic fixture validation failed.");
    if (validated != await store.ReadBackupAsync()) throw new Exception("Equal native backup observations compared unequal.");
    var restored = await store.RestoreBackupAsync(validated.Token);
    if (!restored.Payload!.SequenceEqual(saved.Payload!)) throw new Exception("Opaque bytes changed.");
    var observations = await Task.WhenAll(Enumerable.Range(0, 16).Select(_ => store.LoadAsync()));
    if (observations.Any(value => value != restored) || observations.Append(restored).ToHashSet().Count != 1)
        throw new Exception("Equal native snapshot observations compared unequal.");
    var page = await store.ListSnapshotCandidatesAsync();
    if (page != await store.ListSnapshotCandidatesAsync()) throw new Exception("Equal candidate pages compared unequal.");
    var candidateId = page.Candidates.First(reference => reference.IsSelected == true).Id;
    var candidate = await store.ReadSnapshotCandidateAsync(candidateId);
    if (candidate != await store.ReadSnapshotCandidateAsync(candidateId)) throw new Exception("Equal candidate observations compared unequal.");
    using var cancelled = new CancellationTokenSource();
    cancelled.Cancel();
    try { await store.SaveAsync(restored.Generation, new byte[] { 1 }, cancelled.Token); throw new Exception("Pre-cancelled write succeeded."); }
    catch (OperationCanceledException) { }
    if ((await store.LoadAsync()).Generation != restored.Generation) throw new Exception("Pre-cancelled write changed storage.");
    var oldBackupId = page.Candidates.First(reference => reference.Source == SnapshotCandidateSource.Backup && reference.IsSelected == true).Id;
    var current = await store.SaveAsync(restored.Generation, "{}"u8.ToArray());
    var rebuild = await store.PrepareBackupRebuildAsync(current.Generation!);
    var rebuilt = await store.RebuildBackupAsync(current.Generation!, rebuild);
    var previous = await store.ReadSnapshotCandidateAsync(oldBackupId);
    if (rebuilt.Snapshot != current || previous.Snapshot != validated.Snapshot || previous.IsSelected != false)
        throw new Exception("Explicit reconstruction did not retain the earlier opaque checkpoint.");
    if (!File.ReadAllBytes(metadata).SequenceEqual(metadataBytes))
        throw new Exception("Recovery changed desktop metadata.");
    var destination = Path.Combine(directory, "portable.copy");
    var opaque = new byte[] { 0, 255, 128 };
    var oversized = new byte[] { 0, 255, 128, 1 };
    await ConfigurationRecoveryCopy.WriteAsync(destination, opaque, opaque.Length);
    try { await ConfigurationRecoveryCopy.WriteAsync(destination, oversized, opaque.Length); throw new Exception("Oversized portable write was accepted."); }
    catch (StorageException error) when (error.Failure.Code == ErrorCode.InvalidConfiguration) { }
    try { await ConfigurationRecoveryCopy.WriteAsync(destination, oversized, opaque.Length, cancelled.Token); throw new Exception("Pre-cancelled portable write was accepted."); }
    catch (OperationCanceledException) { }
    var signedDate = DateTime.UnixEpoch.AddTicks(-12_500_000);
    File.SetLastWriteTimeUtc(destination, signedDate);
    var reviewed = await ConfigurationRecoveryCopy.ReadAsync(destination, opaque.Length);
    if (!reviewed.Payload.SequenceEqual(opaque)) throw new Exception("Rejected portable write changed the retained copy.");
    if (reviewed.ModifiedAt != -1.25) throw new Exception("The copy lost its signed fractional date.");
    if (reviewed != await ConfigurationRecoveryCopy.ReadAsync(destination, opaque.Length)) throw new Exception("Equal portable copies compared unequal.");
    File.SetLastWriteTimeUtc(destination, DateTime.UnixEpoch.AddTicks(1));
    var early = await ConfigurationRecoveryCopy.ReadAsync(destination, opaque.Length);
    File.SetLastWriteTimeUtc(destination, DateTime.UnixEpoch.AddTicks(2));
    var later = await ConfigurationRecoveryCopy.ReadAsync(destination, opaque.Length);
    if (early.ModifiedAt != 1e-7 || later.ModifiedAt != 2e-7 || early == later
        || early.Digest != reviewed.Digest || later.Digest != reviewed.Digest)
        throw new Exception("Portable copy observations lost their distinct fractional epoch dates.");
    var portableConfiguration = new Configuration(Path.Combine(directory, "portable-restore"), "com.example.portable-restore");
    using var portableStore = await ConfigurationStore.OpenAsync(portableConfiguration);
    var original = await portableStore.SaveAsync(null, new byte[] { 77 });
    var originalPage = await portableStore.ListSnapshotCandidatesAsync(source: SnapshotCandidateSource.Generation);
    var originalId = originalPage.Candidates.Single().Id;
    var token = await portableStore.PrepareRestoreAsync(reviewed.Payload);
    var replacement = new byte[] { 1, 254, 127 };
    await ConfigurationRecoveryCopy.WriteAsync(destination, replacement);
    var obstruction = Path.Combine(portableConfiguration.Directory, "Recovery");
    const string sentinel = "unavailable recovery directory";
    File.WriteAllText(obstruction, sentinel);
    try
    {
        await portableStore.RestoreSnapshotAsync(reviewed.Payload, token);
        throw new Exception("Restoration published before preserving the original selector.");
    }
    catch (StorageException error) when (error.Failure.Code == ErrorCode.InvalidStoredData) { }
    if (await portableStore.LoadAsync() != original || File.ReadAllText(obstruction) != sentinel)
        throw new Exception("Failed restore changed the original data.");
    File.Delete(obstruction);
    var portableRestored = await portableStore.RestoreSnapshotAsync(reviewed.Payload, token);
    using var reopened = await ConfigurationStore.OpenExistingAsync(portableConfiguration);
    var loaded = await reopened.LoadAsync();
    if (!portableRestored.IsInitialized || loaded.Generation != portableRestored.Generation || !loaded.Payload!.SequenceEqual(opaque))
        throw new Exception("Restoration did not persist the reviewed payload.");
    var originalCandidate = await reopened.ReadSnapshotCandidateAsync(originalId);
    if (originalCandidate.Snapshot != original || originalCandidate.IsSelected != false)
        throw new Exception("Restoration did not retain the original generation.");
    var retained = await ConfigurationRecoveryCopy.ReadAsync(destination);
    if (!retained.Payload.SequenceEqual(replacement) || retained.Digest == reviewed.Digest)
        throw new Exception("Restoration changed the replacement portable copy.");

    var queue = new SnapshotSaveQueue<int>(_ => Task.CompletedTask);
    var idle = queue.State;
    if (idle != queue.State) throw new Exception("Equal idle save queue observations compared unequal.");
    var revision = queue.Enqueue(1);
    var pending = queue.State;
    if (pending != queue.State || pending == idle || pending.PendingRevision != revision || !pending.HasUnsavedChanges)
        throw new Exception("Save queue observations did not reflect pending work.");
    await queue.FlushAsync();
    var committed = queue.State;
    if (committed != queue.State || committed == pending || committed.CommittedRevision != revision || committed.HasUnsavedChanges)
        throw new Exception("Save queue observations did not reflect the committed revision.");
    if (new HashSet<SnapshotSaveState> { idle, pending, committed, queue.State }.Count != 3)
        throw new Exception("Equal save queue observations did not hash consistently.");
}
finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
Console.WriteLine("Clean local NuGet consumer passed token validation, copied bytes, serialized reads and reviewed portable restoration.");
