using System.Text.Json;

namespace NaviApps.ConfigurationStorage;

/// <summary>Serializes this facade's native disk operations away from the UI thread.</summary>
public sealed class ConfigurationStore : IDisposable
{
    private readonly IStorageNative native;
    private readonly Configuration configuration;
    private readonly bool existingOnly;
    private readonly SemaphoreSlim operations = new(1, 1);
    private readonly object lifetime = new();
    private StoreHandle? handle;
    private bool disposed;

    private ConfigurationStore(Configuration configuration, IStorageNative native, bool existingOnly, StoreHandle? handle = null)
    {
        this.configuration = configuration;
        this.native = native;
        this.existingOnly = existingOnly;
        this.handle = handle;
    }

    /// <summary>Creates a facade without opening storage. The first operation opens or claims the configured store.</summary>
    public static ConfigurationStore CreateLazy(Configuration configuration)
        => CreateLazy(configuration, PInvokeStorageNative.Instance, existingOnly: false);

    /// <summary>Creates a facade without opening storage. The first operation requires an existing owned store.</summary>
    public static ConfigurationStore CreateLazyExisting(Configuration configuration)
        => CreateLazy(configuration, PInvokeStorageNative.Instance, existingOnly: true);

    internal static ConfigurationStore CreateLazy(Configuration configuration, IStorageNative native, bool existingOnly = false)
    {
        ArgumentNullException.ThrowIfNull(configuration);
        return new(configuration, native, existingOnly);
    }

    public static Task<ConfigurationStore> OpenAsync(Configuration configuration, CancellationToken cancellationToken = default)
        => OpenAsync(configuration, PInvokeStorageNative.Instance, cancellationToken);

    /// <summary>Opens a store already claimed by the configured owner without creating or claiming it.
    /// An absent store fails with StoreMissing. This does not load or validate its snapshot.</summary>
    public static Task<ConfigurationStore> OpenExistingAsync(Configuration configuration, CancellationToken cancellationToken = default)
        => OpenExistingAsync(configuration, PInvokeStorageNative.Instance, cancellationToken);

    internal static Task<ConfigurationStore> OpenAsync(Configuration configuration, IStorageNative native, CancellationToken cancellationToken = default)
        => OpenCoreAsync(configuration, native, existingOnly: false, cancellationToken);

    internal static Task<ConfigurationStore> OpenExistingAsync(Configuration configuration, IStorageNative native, CancellationToken cancellationToken = default)
        => OpenCoreAsync(configuration, native, existingOnly: true, cancellationToken);

    private static Task<ConfigurationStore> OpenCoreAsync(Configuration configuration, IStorageNative native, bool existingOnly, CancellationToken cancellationToken)
        => Task.Run(() => new ConfigurationStore(
            configuration, native, existingOnly, OpenHandle(configuration, native, existingOnly, cancellationToken)));

    private static StoreHandle OpenHandle(Configuration configuration, IStorageNative native, bool existingOnly, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        ArgumentNullException.ThrowIfNull(configuration);
        Wire.ValidateLocalPath(configuration.Directory);
        var json = Wire.Encode(configuration);
        PInvokeStorageNative.ValidateAbi(native);
        cancellationToken.ThrowIfCancellationRequested();
        IntPtr error;
        var pointer = existingOnly ? native.OpenExisting(json, out error) : native.Open(json, out error);
        var owner = new StoreHandle(pointer, native);
        try
        {
            if (error != IntPtr.Zero)
                return Wire.Consume<StoreHandle>(error, native, 0, static (_, _) => throw Wire.InvalidReply());
            if (owner.IsInvalid) throw Wire.InvalidReply();
            return owner;
        }
        catch { owner.Dispose(); throw; }
    }

    public Task<Snapshot> LoadAsync(CancellationToken cancellationToken = default)
        => ExecuteAsync(new StorageRequest("load"), Wire.Snapshot, cancellationToken);

    /// <summary>Lists unverified generation and backup metadata, including known selected files.
    /// Limit is 1 through 256; after is an opaque exclusive cursor. Concurrent edits may change
    /// later pages; restart from the first page to rescan. This does not select or validate bytes.</summary>
    public Task<SnapshotCandidatePage> ListSnapshotCandidatesAsync(int limit = 64, string? after = null, SnapshotCandidateSource? source = null, CancellationToken cancellationToken = default)
        => ExecuteAsync(new SnapshotCandidatePageRequest("listSnapshotCandidates", limit, after,
            source switch
            {
                null => null,
                SnapshotCandidateSource.Generation => "generation",
                SnapshotCandidateSource.Backup => "backup",
                _ => throw new ArgumentOutOfRangeException(nameof(source))
            }), Wire.CandidatePage, cancellationToken);

    /// <summary>Reads a retained candidate through a private SQLite-family copy without changing
    /// selection or original files. Validate the returned opaque bytes with the host's schema
    /// before using PrepareRestoreAsync and RestoreSnapshotAsync.</summary>
    public Task<SnapshotCandidate> ReadSnapshotCandidateAsync(string id, CancellationToken cancellationToken = default)
        => ExecuteAsync(new SnapshotCandidateRequest("readSnapshotCandidate", id), Wire.Candidate, cancellationToken);

    public Task<Snapshot> SaveAsync(string? expectedGeneration, ReadOnlyMemory<byte> payload, CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested) return Task.FromCanceled<Snapshot>(cancellationToken);
        if (payload.Length > configuration.MaximumPayloadBytes)
            return Task.FromException<Snapshot>(new StorageException(new(ErrorCode.InvalidConfiguration)));
        // Capture before returning so caller mutation cannot change an accepted request.
        var copy = payload.ToArray();
        return ExecuteAsync(new ExpectedGenerationRequest("save", expectedGeneration), Wire.Snapshot, cancellationToken, copy);
    }

    /// <summary>Returns the exact existing backup, or its absence, together with a rotation token.
    /// Validate the returned backup bytes and choose whether to replace them before creating a backup.</summary>
    public Task<BackupPreparation> PrepareBackupAsync(string expectedGeneration, CancellationToken cancellationToken = default)
        => ExecuteAsync(new ExpectedGenerationRequest("prepareBackup", expectedGeneration), Wire.BackupPreparation, cancellationToken);

    /// <summary>Creates or reuses a backup only while the prepared backup state is unchanged.
    /// Supply BackupPreparation.Token; a changed backup or wrong-purpose token fails with BackupChanged,
    /// including when the backup already has the requested generation.</summary>
    public Task<BackupSnapshot> CreateBackupAsync(string expectedGeneration, string token, CancellationToken cancellationToken = default)
        => ExecuteAsync(new BackupCommitRequest("createBackup", expectedGeneration, token), Wire.Backup, cancellationToken);

    /// <summary>Reviews complete raw primary and backup identities for an explicit rebuild.
    /// Requires the current primary generation; a stale generation fails with Conflict.
    /// Preparation does not select a new backup. Known valid
    /// backups with a foreign owner or unsupported format are refused.</summary>
    public Task<string> PrepareBackupRebuildAsync(string expectedGeneration, CancellationToken cancellationToken = default)
        => ExecuteAsync(new ExpectedGenerationRequest("prepareBackupRebuild", expectedGeneration), Wire.ReviewToken, cancellationToken);

    /// <summary>Rechecks reviewed identities and selects a fresh backup, retaining the prior family.
    /// A stale token fails with BackupChanged, including after a newer primary write or any
    /// raw family change, and requires a new review. This explicitly permits rebuilding an invalid
    /// backup; normal CreateBackupAsync remains conservative.</summary>
    public Task<BackupSnapshot> RebuildBackupAsync(string expectedGeneration, string token, CancellationToken cancellationToken = default)
        => ExecuteAsync(new BackupCommitRequest("rebuildBackup", expectedGeneration, token), Wire.Backup, cancellationToken);

    /// <summary>Returns opaque backup bytes for host validation and a token binding the backup
    /// and current raw active selection. Re-read and validate after either changes.</summary>
    public Task<BackupSnapshot> ReadBackupAsync(CancellationToken cancellationToken = default)
        => ExecuteAsync(new StorageRequest("readBackup"), Wire.Backup, cancellationToken);

    /// <summary>Rust rechecks both the backup and raw active selection represented by the
    /// validated token. Newer writes fail with BackupChanged instead of being overwritten.</summary>
    public Task<Snapshot> RestoreBackupAsync(string token, CancellationToken cancellationToken = default)
        => ExecuteAsync(new ReviewTokenRequest("restoreBackup", token), Wire.Snapshot, cancellationToken);

    /// <summary>Binds the caller's opaque bytes and current raw selection for restoration.
    /// The host must validate these bytes before restoring with the returned token.</summary>
    public Task<string> PrepareRestoreAsync(ReadOnlyMemory<byte> payload, CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested) return Task.FromCanceled<string>(cancellationToken);
        if (payload.Length > configuration.MaximumPayloadBytes)
            return Task.FromException<string>(new StorageException(new(ErrorCode.InvalidConfiguration)));
        var copy = payload.ToArray();
        return ExecuteAsync(new StorageRequest("prepareRestore"), Wire.ReviewToken, cancellationToken, copy);
    }

    /// <summary>Restores validated bytes to a fresh generation while retaining original files.
    /// Changed bytes or raw selection fail with RestoreChanged; prepare and validate again.</summary>
    public Task<Snapshot> RestoreSnapshotAsync(ReadOnlyMemory<byte> payload, string token, CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested) return Task.FromCanceled<Snapshot>(cancellationToken);
        if (payload.Length > configuration.MaximumPayloadBytes)
            return Task.FromException<Snapshot>(new StorageException(new(ErrorCode.InvalidConfiguration)));
        var copy = payload.ToArray();
        return ExecuteAsync(new ReviewTokenRequest("restoreSnapshot", token), Wire.Snapshot, cancellationToken, copy);
    }

    private async Task<T> ExecuteAsync<TCommand, T>(TCommand command, Func<JsonElement, byte[]?, T> decode,
        CancellationToken cancellationToken, byte[]? payload = null)
    {
        await operations.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            return await Task.Run(() =>
            {
                cancellationToken.ThrowIfCancellationRequested();
                var json = Wire.Encode(command);
                StoreHandle? current = null;
                bool held = false;
                try
                {
                    lock (lifetime)
                    {
                        ObjectDisposedException.ThrowIf(disposed, this);
                        current = handle;
                        current?.DangerousAddRef(ref held);
                    }
                    if (current is null)
                    {
                        // The operation gate serializes admission. Dispose can still run while
                        // the native open is in progress.
                        current = OpenHandle(configuration, native, existingOnly, cancellationToken);
                        try
                        {
                            lock (lifetime)
                            {
                                ObjectDisposedException.ThrowIf(disposed, this);
                                current.DangerousAddRef(ref held);
                                handle = current;
                            }
                        }
                        catch { current.Dispose(); throw; }
                    }
                    else
                    {
                        cancellationToken.ThrowIfCancellationRequested();
                    }
                    // Once native work begins, preserve the first operation through admission
                    // and dispatch, including its confirmed write or real failure.
                    var result = native.Dispatch(current.DangerousGetHandle(), json, payload);
                    return Wire.Consume(result, native, configuration.MaximumPayloadBytes, decode);
                }
                finally { if (held) current!.DangerousRelease(); }
            }).ConfigureAwait(false);
        }
        finally { operations.Release(); }
    }

    /// <summary>Rejects queued or new work. A dispatch already running retains its native owner until completion.</summary>
    public void Dispose()
    {
        lock (lifetime)
        {
            if (disposed) return;
            disposed = true;
            handle?.Dispose();
        }
    }
}
