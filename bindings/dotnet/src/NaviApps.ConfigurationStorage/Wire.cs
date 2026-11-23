using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace NaviApps.ConfigurationStorage;

internal static class Wire
{
    internal const int MaximumJsonBytes = 96 * 1024 * 1024;
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

    internal static T Consume<T>(IntPtr pointer, IStorageNative native, Func<JsonElement, T> decode)
    {
        if (pointer == IntPtr.Zero) throw InvalidReply();
        uint status;
        byte[] copy;
        using (var owner = new ResultHandle(pointer, native))
        {
            var view = native.View(pointer);
            if (view.Code > 1 || view.Data == IntPtr.Zero || view.Length == 0 || view.Length > MaximumJsonBytes)
                throw InvalidReply();
            status = view.Code;
            copy = new byte[(int)view.Length];
            Marshal.Copy(view.Data, copy, 0, copy.Length);
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
            return decode(root);
        }
        catch (Exception error) when (error is JsonException or InvalidOperationException or KeyNotFoundException)
        {
            throw InvalidReply();
        }
    }

    internal static string ReviewToken(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.String || string.IsNullOrEmpty(root.GetString()))
            throw InvalidReply();
        return root.GetString()!;
    }

    private static byte[] DecodePayload(JsonElement payload)
    {
        // Canonical replies avoid a UTF-16 base64 string. Keep the legacy
        // decoder's accepted forms when the direct decoder rejects a value.
        return payload.TryGetBytesFromBase64(out var bytes)
            ? bytes : Convert.FromBase64String(payload.GetString()!);
    }

    internal static ConfigurationRecoveryCopy RecoveryCopy(JsonElement root, int maximumPayloadBytes)
    {
        try
        {
            var payload = root.GetProperty("payload");
            if (payload.ValueKind is not (JsonValueKind.String or JsonValueKind.Null)) throw InvalidReply();
            var digest = root.GetProperty("digest").GetString();
            var modifiedAt = root.GetProperty("modifiedAt").GetDouble();
            if (payload.ValueKind == JsonValueKind.Null || digest is not { Length: 64 } || !digest.All(char.IsAsciiHexDigitLower)
                || !double.IsFinite(modifiedAt)) throw InvalidReply();
            var bytes = DecodePayload(payload);
            if (bytes.Length > maximumPayloadBytes) throw InvalidReply();
            return new(bytes, digest, modifiedAt);
        }
        catch (Exception error) when (error is InvalidOperationException or KeyNotFoundException or FormatException)
        {
            throw InvalidReply();
        }
    }

    internal static Snapshot Snapshot(JsonElement root, int maximumPayloadBytes)
    {
        try
        {
            var generation = root.GetProperty("generation").GetString();
            var payload = root.GetProperty("payload");
            if (payload.ValueKind is not (JsonValueKind.String or JsonValueKind.Null)) throw InvalidReply();
            var isNull = payload.ValueKind == JsonValueKind.Null;
            if ((generation is null) != isNull
                || generation is { Length: 0 }) throw InvalidReply();
            var bytes = isNull ? null : DecodePayload(payload);
            if (bytes?.Length > maximumPayloadBytes) throw InvalidReply();
            return new(generation, bytes);
        }
        catch (Exception error) when (error is InvalidOperationException or KeyNotFoundException or FormatException)
        {
            throw InvalidReply();
        }
    }

    internal static BackupSnapshot Backup(JsonElement root, int maximumPayloadBytes)
    {
        try
        {
            var snapshot = Snapshot(root.GetProperty("snapshot"), maximumPayloadBytes);
            var token = root.GetProperty("token").GetString();
            var modifiedAt = root.GetProperty("modifiedAt").GetDouble();
            if (!snapshot.IsInitialized || string.IsNullOrEmpty(token) || !double.IsFinite(modifiedAt))
                throw InvalidReply();
            return new(snapshot, token, modifiedAt);
        }
        catch (Exception error) when (error is InvalidOperationException or KeyNotFoundException or FormatException)
        {
            throw InvalidReply();
        }
    }

    internal static BackupPreparation BackupPreparation(JsonElement root, int maximumPayloadBytes)
    {
        try
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
            var token = ReviewToken(root.GetProperty("token"));
            var backup = root.GetProperty("backup");
            return new(backup.ValueKind == JsonValueKind.Null ? null : Backup(backup, maximumPayloadBytes), token);
        }
        catch (Exception error) when (error is InvalidOperationException or KeyNotFoundException or FormatException)
        {
            throw InvalidReply();
        }
    }

    private static string CandidateId(JsonElement value)
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

    internal static SnapshotCandidatePage CandidatePage(JsonElement root)
    {
        try
        {
            var values = root.GetProperty("candidates");
            var count = values.GetArrayLength();
            if (count > 256) throw InvalidReply();
            var candidates = new SnapshotCandidateReference[count];
            for (var index = 0; index < count; index++)
            {
                var value = values[index];
                var id = CandidateId(value.GetProperty("id"));
                var isSelected = CandidateSelection(value.GetProperty("isSelected"));
                if (index > 0 && string.CompareOrdinal(candidates[index - 1].Id, id) >= 0)
                    throw InvalidReply();
                candidates[index] = new(id, CandidateSource(value.GetProperty("source")), isSelected);
            }
            var cursor = root.GetProperty("nextCursor");
            var nextCursor = cursor.ValueKind == JsonValueKind.Null ? null : CandidateId(cursor);
            if (nextCursor is not null && (count == 0 || nextCursor != candidates[^1].Id))
                throw InvalidReply();
            return new(candidates, nextCursor);
        }
        catch (Exception error) when (error is InvalidOperationException or KeyNotFoundException or FormatException)
        {
            throw InvalidReply();
        }
    }

    internal static SnapshotCandidate Candidate(JsonElement root, int maximumPayloadBytes)
    {
        try
        {
            var id = CandidateId(root.GetProperty("id"));
            var snapshot = Snapshot(root.GetProperty("snapshot"), maximumPayloadBytes);
            var modifiedAt = root.GetProperty("modifiedAt").GetDouble();
            var isSelected = CandidateSelection(root.GetProperty("isSelected"));
            if (!snapshot.IsInitialized || !double.IsFinite(modifiedAt)) throw InvalidReply();
            return new(id, CandidateSource(root.GetProperty("source")), snapshot, modifiedAt, isSelected);
        }
        catch (Exception error) when (error is InvalidOperationException or KeyNotFoundException or FormatException)
        {
            throw InvalidReply();
        }
    }
}
