using System.Text.Json;

namespace NaviApps.ConfigurationStorage;

/// <summary>Owned opaque bytes from a portable, integrity-protected recovery file,
/// independent of any store directory or owner. The host owns semantic validation.</summary>
public sealed class ConfigurationRecoveryCopy : IEquatable<ConfigurationRecoveryCopy>
{
    public const int DefaultMaximumPayloadBytes = 16 * 1024 * 1024;
    public const int MaximumSupportedPayloadBytes = Configuration.MaximumSupportedPayloadBytes;
    private readonly byte[] payload;

    /// <summary>Returns a fresh copy of the opaque payload, including empty bytes.</summary>
    public byte[] Payload => payload.ToArray();
    /// <summary>Hex digest of the format header and payload. Integrity, not authentication.</summary>
    public string Digest { get; }
    /// <summary>File modification time in Unix seconds, including the fractional part.</summary>
    public double ModifiedAt { get; }

    internal ConfigurationRecoveryCopy(byte[] payload, string digest, double modifiedAt)
    {
        this.payload = payload.ToArray();
        Digest = digest;
        ModifiedAt = modifiedAt;
    }

    public bool Equals(ConfigurationRecoveryCopy? other) => other is not null
        && Digest == other.Digest
        && ModifiedAt.Equals(other.ModifiedAt)
        && payload.AsSpan().SequenceEqual(other.payload);

    public override bool Equals(object? obj) => obj is ConfigurationRecoveryCopy other && Equals(other);

    public override int GetHashCode() => HashCode.Combine(Digest, ModifiedAt);

    public static bool operator ==(ConfigurationRecoveryCopy? left, ConfigurationRecoveryCopy? right) => left?.Equals(right) ?? right is null;
    public static bool operator !=(ConfigurationRecoveryCopy? left, ConfigurationRecoveryCopy? right) => !(left == right);

    public static Task<ConfigurationRecoveryCopy> ReadAsync(string path,
        int maximumPayloadBytes = DefaultMaximumPayloadBytes, CancellationToken cancellationToken = default)
        => ReadAsync(path, maximumPayloadBytes, PInvokeStorageNative.Instance, cancellationToken);

    internal static async Task<ConfigurationRecoveryCopy> ReadAsync(string path, int maximumPayloadBytes,
        IStorageNative native, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        Validate(path, maximumPayloadBytes);
        var result = await ExecuteAsync(new { command = "read", path, maximumPayloadBytes }, native, cancellationToken).ConfigureAwait(false);
        return Wire.RecoveryCopy(result, maximumPayloadBytes);
    }

    /// <summary>Captures caller bytes and atomically replaces the explicitly selected regular
    /// file. Final links are rejected. The host must protect its own managed paths.</summary>
    public static Task WriteAsync(string path, ReadOnlyMemory<byte> payload,
        int maximumPayloadBytes = DefaultMaximumPayloadBytes, CancellationToken cancellationToken = default)
        => WriteAsync(path, payload, maximumPayloadBytes, PInvokeStorageNative.Instance, cancellationToken);

    internal static async Task WriteAsync(string path, ReadOnlyMemory<byte> payload, int maximumPayloadBytes,
        IStorageNative native, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        Validate(path, maximumPayloadBytes);
        if (payload.Length > maximumPayloadBytes) throw new StorageException(new(ErrorCode.InvalidConfiguration));
        var copy = payload.ToArray();
        var result = await ExecuteAsync(new { command = "write", path, maximumPayloadBytes, payload = copy }, native, cancellationToken).ConfigureAwait(false);
        if (result.ValueKind != JsonValueKind.Null) throw Wire.InvalidReply();
    }

    private static void Validate(string path, int maximumPayloadBytes)
    {
        Wire.ValidateLocalPath(path);
        if (maximumPayloadBytes is < 1 or > MaximumSupportedPayloadBytes)
            throw new StorageException(new(ErrorCode.InvalidConfiguration));
    }

    private static Task<JsonElement> ExecuteAsync<TCommand>(TCommand command, IStorageNative native, CancellationToken cancellationToken)
        => Task.Run(() =>
        {
            cancellationToken.ThrowIfCancellationRequested();
            var json = Wire.Encode(command);
            PInvokeStorageNative.ValidateAbi(native);
            cancellationToken.ThrowIfCancellationRequested();
            // Once dispatch starts, deliver its outcome even if cancellation arrives later.
            return Wire.Consume(native.RecoveryCopyDispatch(json), native);
        });
}
