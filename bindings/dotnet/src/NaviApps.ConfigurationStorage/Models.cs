namespace NaviApps.ConfigurationStorage;

public sealed record Configuration(string Directory, string Owner, int MaximumPayloadBytes = 16 * 1024 * 1024)
{
    public const int MaximumSupportedPayloadBytes = 64 * 1024 * 1024;
}

public enum ErrorCode
{
    InvalidConfiguration, OwnerMismatch, InvalidStoredData,
    /// <summary>The stored SQLite format version is unsupported, independently of the native ABI.</summary>
    UnsupportedFormatVersion,
    Conflict, Busy, NotInitialized, BackupMissing, BackupChanged, IoFailure,
    /// <summary>The native library ABI differs from the binding's supported ABI.</summary>
    UnsupportedAbiVersion,
    /// <summary>The portable recovery copy failed framing or integrity validation.</summary>
    InvalidRecoveryCopy,
    /// <summary>The recovery-copy file version is unsupported, independently of SQLite and ABI versions.</summary>
    UnsupportedRecoveryVersion,
    /// <summary>The proposed bytes or current raw selection changed after preparation.</summary>
    RestoreChanged,
    /// <summary>The retained snapshot candidate no longer exists.</summary>
    SnapshotCandidateMissing,
    /// <summary>Raw recovery bytes or managed backup member count exceed the limit.</summary>
    RawRecoveryLimitExceeded,
    /// <summary>No directory exists at the requested store location.</summary>
    StoreMissing
}

public sealed record Failure(ErrorCode Code, string? RecoveryDirectory = null);

public sealed class StorageException(Failure failure) : Exception(failure.Code.ToString())
{
    public Failure Failure { get; } = failure;
}

/// <summary>Opaque owned bytes. A null generation and payload mean uninitialized.</summary>
public sealed class Snapshot : IEquatable<Snapshot>
{
    private readonly byte[]? payload;
    public string? Generation { get; }
    public bool IsInitialized => Generation is not null;
    /// <summary>Returns a fresh copy; initialized empty bytes remain distinct from null.</summary>
    public byte[]? Payload => payload?.ToArray();

    internal Snapshot(string? generation, byte[]? payload)
    {
        Generation = generation;
        this.payload = payload?.ToArray();
    }

    public bool Equals(Snapshot? other) => other is not null
        && Generation == other.Generation
        && (payload is null) == (other.payload is null)
        && payload.AsSpan().SequenceEqual(other.payload);

    public override bool Equals(object? obj) => obj is Snapshot other && Equals(other);

    // The native generation binds the content; hashing can avoid rescanning the payload.
    public override int GetHashCode() => HashCode.Combine(Generation, payload?.Length);

    public static bool operator ==(Snapshot? left, Snapshot? right) => left?.Equals(right) ?? right is null;
    public static bool operator !=(Snapshot? left, Snapshot? right) => !(left == right);
}

/// <param name="ModifiedAt">Unix seconds, including the fractional part.</param>
public sealed record BackupSnapshot(Snapshot Snapshot, string Token, double ModifiedAt);

/// <summary>The exact backup, or its absence, observed before a guarded backup rotation.</summary>
/// <param name="Backup">A backup for host validation, or null when no backup exists.</param>
/// <param name="Token">Opaque rotation authorization. This is distinct from Backup.Token, which is for restoration.</param>
public sealed record BackupPreparation(BackupSnapshot? Backup, string Token);

/// <summary>The managed location of a candidate.</summary>
public enum SnapshotCandidateSource { Generation, Backup }

/// <summary>Unverified retained-file metadata. A null selection means the source selection cannot be determined.</summary>
public sealed record SnapshotCandidateReference(string Id, SnapshotCandidateSource Source, bool? IsSelected);

/// <summary>An immutable page of unverified retained-file metadata, ordered by opaque ID.</summary>
public sealed class SnapshotCandidatePage : IEquatable<SnapshotCandidatePage>
{
    public IReadOnlyList<SnapshotCandidateReference> Candidates { get; }
    public string? NextCursor { get; }

    internal SnapshotCandidatePage(IEnumerable<SnapshotCandidateReference> candidates, string? nextCursor)
    {
        Candidates = Array.AsReadOnly(candidates.ToArray());
        NextCursor = nextCursor;
    }

    public bool Equals(SnapshotCandidatePage? other) => other is not null
        && NextCursor == other.NextCursor
        && Candidates.SequenceEqual(other.Candidates);

    public override bool Equals(object? obj) => obj is SnapshotCandidatePage other && Equals(other);

    public override int GetHashCode()
    {
        var hash = new HashCode();
        hash.Add(NextCursor);
        foreach (var candidate in Candidates) hash.Add(candidate);
        return hash.ToHashCode();
    }

    public static bool operator ==(SnapshotCandidatePage? left, SnapshotCandidatePage? right) => left?.Equals(right) ?? right is null;
    public static bool operator !=(SnapshotCandidatePage? left, SnapshotCandidatePage? right) => !(left == right);
}

/// <summary>A storage-verified snapshot whose opaque payload still requires host semantic validation.</summary>
/// <param name="Id">Opaque candidate ID, independent of the snapshot's generation token.</param>
/// <param name="Source">The managed location of the candidate.</param>
/// <param name="Snapshot">An initialized snapshot with owned payload bytes.</param>
/// <param name="ModifiedAt">Finite signed Unix seconds, including the fractional part.</param>
/// <param name="IsSelected">Null when the source selection cannot be determined, including an unreadable selector.</param>
public sealed record SnapshotCandidate(string Id, SnapshotCandidateSource Source, Snapshot Snapshot, double ModifiedAt, bool? IsSelected);
