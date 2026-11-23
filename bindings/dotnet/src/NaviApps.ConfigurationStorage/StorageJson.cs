using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

namespace NaviApps.ConfigurationStorage;

internal sealed record StorageRequest(string Command);
internal sealed record ExpectedGenerationRequest(string Command, string? ExpectedGeneration);
internal sealed record BackupCommitRequest(string Command, string ExpectedGeneration, string Token);
internal sealed record ReviewTokenRequest(string Command, string Token);
internal sealed record SnapshotCandidateRequest(string Command, string Id);
internal sealed record SnapshotCandidatePageRequest(string Command, int Limit, string? After, string? Source);
internal sealed record RecoveryCopyRequest(string Command, string Path, int MaximumPayloadBytes);

[JsonSourceGenerationOptions(GenerationMode = JsonSourceGenerationMode.Metadata, PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
[JsonSerializable(typeof(Configuration))]
[JsonSerializable(typeof(StorageRequest))]
[JsonSerializable(typeof(ExpectedGenerationRequest))]
[JsonSerializable(typeof(BackupCommitRequest))]
[JsonSerializable(typeof(ReviewTokenRequest))]
[JsonSerializable(typeof(SnapshotCandidateRequest))]
[JsonSerializable(typeof(SnapshotCandidatePageRequest))]
[JsonSerializable(typeof(RecoveryCopyRequest))]
internal sealed partial class StorageJson : JsonSerializerContext
{
    internal static JsonTypeInfo<T> Type<T>() => (JsonTypeInfo<T>)Default.GetTypeInfo(typeof(T))!;
}
