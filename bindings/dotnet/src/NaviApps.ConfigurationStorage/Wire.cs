using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace NaviApps.ConfigurationStorage;

internal static class Wire
{
    internal const int MaximumJsonBytes = 1024 * 1024;
    private static readonly JsonSerializerOptions Options = new() { PropertyNamingPolicy = JsonNamingPolicy.CamelCase };
    private static readonly UTF8Encoding StrictUtf8 = new(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true);

    internal static void ValidateLocalPath(string path)
    {
        if (string.IsNullOrEmpty(path) || !Path.IsPathFullyQualified(path) || path.Contains('\0'))
            throw new StorageException(new(ErrorCode.InvalidConfiguration));
        try
        {
            // JSON would silently replace an unpaired UTF-16 surrogate, selecting a different path.
            _ = StrictUtf8.GetByteCount(path);
        }
        catch (EncoderFallbackException)
        {
            throw new StorageException(new(ErrorCode.InvalidConfiguration));
        }
    }

    internal static byte[] Encode<T>(T value)
    {
        var bytes = JsonSerializer.SerializeToUtf8Bytes(value, Options);
        if (bytes.Length > MaximumJsonBytes) throw new StorageException(new(ErrorCode.InvalidConfiguration));
        return bytes;
    }

    internal static InvalidDataException InvalidReply() => new("Invalid configuration_storage ABI reply.");

    internal static T Consume<T>(IntPtr pointer, IStorageNative native, int maximumPayloadBytes,
        Func<JsonElement, byte[]?, T> decode)
    {
        if (pointer == IntPtr.Zero) throw InvalidReply();
        uint status;
        byte[] copy;
        byte[]? payload = null;
        using (var owner = new ResultHandle(pointer, native))
        {
            var view = native.View(pointer);
            var limit = (nuint)Math.Clamp(maximumPayloadBytes, 0, Configuration.MaximumSupportedPayloadBytes);
            if (view.Code > 1 || view.Data == IntPtr.Zero || view.Length == 0 || view.Length > MaximumJsonBytes
                || view.PayloadLength > limit || (view.Payload == IntPtr.Zero && view.PayloadLength != 0)
                || (view.Code != 0 && view.Payload != IntPtr.Zero))
                throw InvalidReply();
            status = view.Code;
            copy = new byte[(int)view.Length];
            Marshal.Copy(view.Data, copy, 0, copy.Length);
            if (view.Payload != IntPtr.Zero)
            {
                payload = new byte[(int)view.PayloadLength];
                if (payload.Length != 0) Marshal.Copy(view.Payload, payload, 0, payload.Length);
            }
        }
        try
        {
            using var document = JsonDocument.Parse(copy);
            var root = document.RootElement;
            if (status != 0)
            {
                if (root.ValueKind != JsonValueKind.Object) throw InvalidReply();
                var code = root.GetProperty("code").GetString() switch
                {
                    "invalidConfiguration" => ErrorCode.InvalidConfiguration,
                    "ownerMismatch" => ErrorCode.OwnerMismatch,
                    "storeMissing" => ErrorCode.StoreMissing,
                    "invalidStoredData" => ErrorCode.InvalidStoredData,
                    "unsupportedFormatVersion" => ErrorCode.UnsupportedFormatVersion,
                    "conflict" => ErrorCode.Conflict,
                    "busy" => ErrorCode.Busy,
                    "notInitialized" => ErrorCode.NotInitialized,
                    "backupMissing" => ErrorCode.BackupMissing,
                    "backupChanged" => ErrorCode.BackupChanged,
                    "invalidRecoveryCopy" => ErrorCode.InvalidRecoveryCopy,
                    "unsupportedRecoveryVersion" => ErrorCode.UnsupportedRecoveryVersion,
                    "restoreChanged" => ErrorCode.RestoreChanged,
                    "snapshotCandidateMissing" => ErrorCode.SnapshotCandidateMissing,
                    "rawRecoveryLimitExceeded" => ErrorCode.RawRecoveryLimitExceeded,
                    "ioFailure" => ErrorCode.IoFailure,
                    _ => throw InvalidReply()
                };
                var recovery = root.TryGetProperty("recoveryDirectory", out var value) ? value.GetString() : null;
                throw new StorageException(new(code, recovery));
            }
            return decode(root, payload);
        }
        catch (Exception error) when (error is JsonException or InvalidOperationException or KeyNotFoundException or FormatException)
        {
            throw InvalidReply();
        }
    }

    internal static string ReviewToken(JsonElement root, byte[]? payload)
    {
        return payload is null ? NonEmptyString(root) : throw InvalidReply();
    }

    internal static ConfigurationRecoveryCopy RecoveryCopy(JsonElement root, byte[]? payload)
    {
        var digest = root.GetProperty("digest").GetString();
        var modifiedAt = root.GetProperty("modifiedAt").GetDouble();
        if (root.TryGetProperty("payload", out _) || payload is null || digest is not { Length: 64 }
            || !digest.All(char.IsAsciiHexDigitLower) || !double.IsFinite(modifiedAt))
            throw InvalidReply();
        return new(payload, digest, modifiedAt);
    }

    internal static Snapshot Snapshot(JsonElement root, byte[]? payload)
    {
        var generation = root.GetProperty("generation").GetString();
        if (root.TryGetProperty("payload", out _) || (generation is null) != (payload is null) || generation is { Length: 0 })
            throw InvalidReply();
        return new(generation, payload);
    }

    internal static BackupSnapshot Backup(JsonElement root, byte[]? payload)
    {
        var snapshot = Snapshot(root.GetProperty("snapshot"), payload);
        var token = root.GetProperty("token").GetString();
        var modifiedAt = root.GetProperty("modifiedAt").GetDouble();
        if (!snapshot.IsInitialized || string.IsNullOrEmpty(token) || !double.IsFinite(modifiedAt))
            throw InvalidReply();
        return new(snapshot, token, modifiedAt);
    }

    internal static BackupPreparation BackupPreparation(JsonElement root, byte[]? payload)
    {
        if (root.ValueKind != JsonValueKind.Object) throw InvalidReply();
        var backupFields = 0;
        var tokenFields = 0;
        foreach (var property in root.EnumerateObject())
        {
            if (property.Name == "backup") backupFields++;
            else if (property.Name == "token") tokenFields++;
        }
        if (backupFields != 1 || tokenFields != 1) throw InvalidReply();
        var token = NonEmptyString(root.GetProperty("token"));
        var backup = root.GetProperty("backup");
        if (backup.ValueKind == JsonValueKind.Null)
            return payload is null ? new(null, token) : throw InvalidReply();
        return new(Backup(backup, payload), token);
    }

    private static string NonEmptyString(JsonElement value)
    {
        if (value.ValueKind != JsonValueKind.String || string.IsNullOrEmpty(value.GetString()))
            throw InvalidReply();
        return value.GetString()!;
    }

    private static SnapshotCandidateSource CandidateSource(JsonElement value) => value.GetString() switch
    {
        "generation" => SnapshotCandidateSource.Generation,
        "backup" => SnapshotCandidateSource.Backup,
        _ => throw InvalidReply()
    };

    private static bool? CandidateSelection(JsonElement value) => value.ValueKind switch
    {
        JsonValueKind.True => true,
        JsonValueKind.False => false,
        JsonValueKind.Null => null,
        _ => throw InvalidReply()
    };

    internal static SnapshotCandidatePage CandidatePage(JsonElement root, byte[]? payload)
    {
        var values = root.GetProperty("candidates");
        var count = values.GetArrayLength();
        if (payload is not null || count > 256) throw InvalidReply();
        var candidates = new SnapshotCandidateReference[count];
        for (var index = 0; index < count; index++)
        {
            var value = values[index];
            var id = NonEmptyString(value.GetProperty("id"));
            var isSelected = CandidateSelection(value.GetProperty("isSelected"));
            if (index > 0 && string.CompareOrdinal(candidates[index - 1].Id, id) >= 0)
                throw InvalidReply();
            candidates[index] = new(id, CandidateSource(value.GetProperty("source")), isSelected);
        }
        var cursor = root.GetProperty("nextCursor");
        var nextCursor = cursor.ValueKind == JsonValueKind.Null ? null : NonEmptyString(cursor);
        if (nextCursor is not null && (count == 0 || nextCursor != candidates[^1].Id))
            throw InvalidReply();
        return new(candidates, nextCursor);
    }

    internal static SnapshotCandidate Candidate(JsonElement root, byte[]? payload)
    {
        var id = NonEmptyString(root.GetProperty("id"));
        var snapshot = Snapshot(root.GetProperty("snapshot"), payload);
        var modifiedAt = root.GetProperty("modifiedAt").GetDouble();
        var isSelected = CandidateSelection(root.GetProperty("isSelected"));
        if (!snapshot.IsInitialized || !double.IsFinite(modifiedAt)) throw InvalidReply();
        return new(id, CandidateSource(root.GetProperty("source")), snapshot, modifiedAt, isSelected);
    }
}
