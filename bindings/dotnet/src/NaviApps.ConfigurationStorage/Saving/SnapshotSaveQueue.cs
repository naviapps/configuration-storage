namespace NaviApps.ConfigurationStorage;

/// <summary>A coherent client-write observation with value equality, independent of disk generation tokens.</summary>
public sealed record SnapshotSaveState
{
    /// <summary>Latest accepted client revision.</summary>
    public ulong Revision { get; }
    /// <summary>Last successfully committed client revision.</summary>
    public ulong CommittedRevision { get; }
    /// <summary>Latest pending revision, if any.</summary>
    public ulong? PendingRevision { get; }
    /// <summary>The revision whose owned commit is in flight.</summary>
    public ulong? WritingRevision { get; }
    /// <summary>Current failed revision retained for explicit retry.</summary>
    public ulong? FailedRevision { get; }
    /// <summary>Whether pending or in-flight work remains.</summary>
    public bool HasUnsavedChanges { get; }
    internal SnapshotSaveState(ConfigurationStorageSaveQueueView value)
    {
        Revision = value.Revision;
        CommittedRevision = value.CommittedRevision;
        PendingRevision = value.PendingRevision == 0 ? null : value.PendingRevision;
        WritingRevision = value.WritingRevision == 0 ? null : value.WritingRevision;
        FailedRevision = value.FailedRevision == 0 ? null : value.FailedRevision;
        HasUnsavedChanges = value.HasUnsavedChanges != 0;
    }
}

/// <summary>Opt-in latest-snapshot coalescing. Enqueue performs no I/O. The host
/// chooses flush, debounce, retry and UI policy. Snapshots must be immutable or
/// copied by the host before enqueueing. A flush caller cannot cancel an owned write.</summary>
[System.Diagnostics.CodeAnalysis.SuppressMessage("Naming", "CA1711:Identifiers should not have incorrect suffix", Justification = "Queue describes the public snapshot-coalescing contract, not a BCL collection.")]
public sealed class SnapshotSaveQueue<TSnapshot>
{
    private sealed record Pending(ulong Revision, TSnapshot Snapshot);
    private readonly object sync = new();
    private readonly SaveQueueHandle owner;
    private readonly Func<TSnapshot, Task> commit;
    private Pending? pending;
    private Task? saving;

    /// <summary>Creates an empty queue without running a timer, callback or disk operation.</summary>
    public SnapshotSaveQueue(Func<TSnapshot, Task> commit)
    {
        ArgumentNullException.ThrowIfNull(commit);
        this.commit = commit;
        owner = PInvokeStorageNative.CreateSaveQueue();
    }

    /// <summary>Returns a single Rust-owned observation without advancing state.</summary>
    public SnapshotSaveState State
    {
        get { lock (sync) return new(PInvokeStorageNative.ViewSaveQueue(owner)); }
    }

    /// <summary>Replaces the pending snapshot without cancelling an in-flight write.
    /// Revision exhaustion throws before changing pending work; retain the rejected snapshot.</summary>
    public ulong Enqueue(TSnapshot snapshot)
    {
        lock (sync)
        {
            var revision = PInvokeStorageNative.EnqueueSaveQueue(owner);
            if (revision == 0) throw new InvalidOperationException("Client save revision space exhausted; retain the unsaved snapshot.");
            pending = new(revision, snapshot);
            return revision;
        }
    }

    /// <summary>Drains the latest snapshots, sharing one task among concurrent
    /// flush callers. A current failure is retained for explicit retry; an older
    /// failure cannot restore obsolete edits over a newer pending snapshot.</summary>
    public Task FlushAsync()
    {
        lock (sync)
        {
            if (saving is not null) return saving;
            if (pending is null) return Task.CompletedTask;
            var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            saving = completion.Task;
            _ = Task.Run(() => DrainAsync(completion));
            return saving;
        }
    }

    private async Task DrainAsync(TaskCompletionSource completion)
    {
        Exception? failure = null;
        try
        {
            while (true)
            {
                Pending next;
                lock (sync)
                {
                    var revision = PInvokeStorageNative.BeginSaveQueue(owner);
                    if (revision == 0)
                    {
                        if (ReferenceEquals(saving, completion.Task)) saving = null;
                        break;
                    }
                    next = pending is { } value && value.Revision == revision ? value
                        : throw new InvalidOperationException("Native save queue and retained snapshot differ.");
                    pending = null;
                }
                try
                {
                    await commit(next.Snapshot).ConfigureAwait(false);
                }
                catch
                {
                    uint result;
                    lock (sync)
                    {
                        result = PInvokeStorageNative.CompleteSaveQueue(owner, next.Revision, 0);
                        if (result == 2)
                        {
                            pending = next;
                            if (ReferenceEquals(saving, completion.Task)) saving = null;
                        }
                    }
                    if (result == 2) throw;
                    if (result != 3) throw new InvalidOperationException("Native queue rejected a failed write acknowledgement.");
                    continue;
                }
                lock (sync)
                    if (PInvokeStorageNative.CompleteSaveQueue(owner, next.Revision, 1) != 1)
                        throw new InvalidOperationException("Native queue rejected a successful write acknowledgement.");
            }
        }
        catch (Exception error) { failure = error; }
        // Detach before delivery: a resumed caller can immediately retry the
        // retained snapshot instead of accidentally joining the failed old task.
        lock (sync)
            if (ReferenceEquals(saving, completion.Task)) saving = null;
        if (failure is null) completion.SetResult();
        else completion.SetException(failure);
    }
}
