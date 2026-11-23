using System.Collections.Concurrent;
using NaviApps.ConfigurationStorage;

static class SnapshotSaveQueueTests
{
    private static void Check(bool condition, string message)
    { if (!condition) throw new InvalidOperationException(message); }

    internal static async Task RunAsync()
    {
        var values = new List<int>();
        var queue = new SnapshotSaveQueue<int>(value => { values.Add(value); return Task.CompletedTask; });
        queue.Enqueue(1);
        queue.Enqueue(2);
        Check(values.Count == 0, "Enqueue performed an automatic write");
        await queue.FlushAsync();
        Check(values is [2] && queue.State.CommittedRevision == 2 && !queue.State.HasUnsavedChanges,
            "Burst coalescing or explicit flush state");
        await queue.FlushAsync();
        Check(values.Count == 1, "Empty flush wrote data");

        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        values.Clear();
        queue = new SnapshotSaveQueue<int>(async value =>
        {
            values.Add(value);
            if (value == 1) { started.SetResult(); await release.Task; throw new IOException("older write"); }
        });
        queue.Enqueue(1);
        var first = queue.FlushAsync();
        await started.Task.WaitAsync(TimeSpan.FromSeconds(5));
        queue.Enqueue(2);
        queue.Enqueue(3);
        var joined = queue.FlushAsync();
        Check(ReferenceEquals(first, joined), "Concurrent flushes did not share their owned writer");
        release.SetResult();
        await Task.WhenAll(first, joined).WaitAsync(TimeSpan.FromSeconds(5));
        Check(values is [1, 3] && queue.State.CommittedRevision == 3 && !queue.State.HasUnsavedChanges,
            "Older failed write discarded or restored an obsolete snapshot");

        var attempts = 0;
        queue = new SnapshotSaveQueue<int>(_ => ++attempts == 1
            ? Task.FromException(new IOException("current write")) : Task.CompletedTask);
        queue.Enqueue(7);
        try { await queue.FlushAsync(); throw new InvalidOperationException("Expected current failure"); }
        catch (IOException) { }
        Check(attempts == 1 && queue.State.FailedRevision == 1 && queue.State.PendingRevision == 1 && queue.State.HasUnsavedChanges,
            "Current failure was lost or automatically retried");
        await queue.FlushAsync();
        Check(attempts == 2 && queue.State.CommittedRevision == 1 && !queue.State.HasUnsavedChanges,
            "Immediate explicit retry joined a stale failed task");

        var observed = new ConcurrentQueue<int>();
        queue = new SnapshotSaveQueue<int>(value => { observed.Enqueue(value); return Task.CompletedTask; });
        var producers = Enumerable.Range(0, 64).Select(_ => Task.Run(async () =>
        {
            queue.Enqueue(1);
            await queue.FlushAsync();
        })).ToArray();
        await Task.WhenAll(producers).WaitAsync(TimeSpan.FromSeconds(5));
        await queue.FlushAsync();
        Check(queue.State.CommittedRevision == 64 && !queue.State.HasUnsavedChanges && !observed.IsEmpty,
            "Concurrent enqueue/flush stranded pending work at writer detachment");
        Console.WriteLine("Rust-owned save coalescing, failure retention, shared flush and concurrent detachment checks passed.");
    }
}
