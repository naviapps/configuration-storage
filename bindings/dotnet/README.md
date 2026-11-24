# NaviApps.ConfigurationStorage

.NET 10 bindings for transactional storage of opaque byte snapshots. The host
owns payload schemas, validation, migrations and save timing.

## Install

Download the package and its receipt for your chosen revision from the
[SDK distribution checks](https://github.com/naviapps/sdk-tooling/actions/workflows/check-sdk-distribution.yml).
Verify the package's SHA-256 against the receipt. The package supports macOS 26
or later on ARM64 and x64, and Windows 11 with x64 .NET, including x64 .NET on
Windows ARM64.

From a new project directory, use the version in the package filename and a
local feed containing the unpacked distribution artifact.

```sh
dotnet new console --framework net10.0 --no-restore
dotnet package add NaviApps.ConfigurationStorage --version "[0.1.0]" --no-restore
dotnet restore --source "/absolute/path/to/sdk-packages" --packages ./.nuget-packages
```

Use a fresh cache whenever you switch candidate builds with the same version.
Replace `Program.cs` with the following example and run `dotnet run --no-restore`.

## Open and save opaque snapshots

```csharp
using NaviApps.ConfigurationStorage;

string directory = Path.Combine(Path.GetTempPath(), $"configuration-storage-{Guid.NewGuid():N}");
using var store = ConfigurationStore.CreateLazy(new Configuration(directory, "sample.configuration"));
var current = await store.LoadAsync();
await store.SaveAsync(current.Generation, "My configuration"u8.ToArray());
var saved = await store.LoadAsync();
Console.WriteLine(System.Text.Encoding.UTF8.GetString(saved.Payload!));
```

The example writes in a unique temporary directory. Supply an explicit absolute
path in your application. Paths must encode to UTF-8 without replacement. The
owner is a nonempty ASCII identifier of up to 1024 characters using letters,
digits, `.`, `_` or `-`. `MaximumPayloadBytes` defaults to 16 MiB and accepts
1 byte through 64 MiB. The limit also applies to returned payloads.

`CreateLazy` and `CreateLazyExisting` defer native admission until the first
operation and retry failed admission on the next operation. Use `OpenAsync`
or `OpenExistingAsync` to check admission immediately. Existing-only admission
requires an owned root and an existing lock on every operation. An absent root
reports `StoreMissing`, an unclaimed or foreign root reports `OwnerMismatch`,
and malformed ownership metadata or a missing owned lock reports
`InvalidStoredData`. Opening checks ownership and locking. Loading checks
the snapshot.

An uninitialized snapshot has null `Generation` and `Payload`. An initialized
empty snapshot has a generation and a non-null empty payload. Pass the opaque
generation from the edited snapshot to the next save. A stale generation reports
`Conflict`. A later observation leaves the host's editing baseline intact.

Input bytes are captured before the asynchronous method returns. Each `Payload`
getter returns a fresh copy. Snapshots compare generation and payload bytes by
value. Backup and candidate types also include their metadata, candidate pages
include the ordered references and cursor, and recovery copies include the
digest and modification time.

## Backup review and restoration

The [recovery contract](https://github.com/naviapps/configuration-storage/blob/main/docs/RECOVERY.md#backup-validation-and-restore)
describes the shared token checks and data-retention guarantees. The .NET
methods return typed results and use the same rules.

- Call `PrepareBackupAsync(expectedGeneration)` and review the returned
  `BackupPreparation.Backup` or its absence. Pass the outer
  `BackupPreparation.Token` to `CreateBackupAsync` after approval.
- Call `ReadBackupAsync` and validate its snapshot with the host's schema.
  Pass the backup's own token to `RestoreBackupAsync` after confirmation.
- Use `PrepareBackupRebuildAsync` and `RebuildBackupAsync` to
  [reconstruct a damaged checkpoint](https://github.com/naviapps/configuration-storage/blob/main/docs/RECOVERY.md#reconstruct-a-damaged-checkpoint).
  Review the current primary payload before requesting reconstruction.

`Conflict` reports a stale generation during preparation. On `BackupChanged`,
read and review the state again before retrying. Restore returns a new generation.

## Inspect retained snapshots

Use `ListSnapshotCandidatesAsync` and `ReadSnapshotCandidateAsync`.
`SnapshotCandidatePage` contains immutable references and an optional
`NextCursor`. Pass that cursor back as `after` with the same
`SnapshotCandidateSource` if you scoped the scan. Treat IDs and cursors as
opaque strings. `SnapshotCandidate.IsSelected` is nullable and describes
selection within its source. The [inspection contract](https://github.com/naviapps/configuration-storage/blob/main/docs/RECOVERY.md#inspect-retained-snapshots)
explains what listing and verified reading guarantee.

Validate the inspected payload, call `PrepareRestoreAsync` before asking for
confirmation, and pass the same payload and token to `RestoreSnapshotAsync`
after approval. On `RestoreChanged`, review the payload and current selection
again. `SnapshotCandidateMissing` identifies an absent candidate.
`RawRecoveryLimitExceeded` identifies an inspection that exceeds the raw file
budget.

## Portable recovery copies

`ConfigurationRecoveryCopy.WriteAsync` and `ReadAsync` operate on an explicit
absolute path outside the store API. Both take `maximumPayloadBytes` and an
optional cancellation token. Writes return a `Task` without metadata. Reads
return an immutable payload, a 64-character hexadecimal `Digest`, and
`ModifiedAt`. Modification times on backups, candidates and recovery copies
are signed fractional Unix seconds.

The [portable recovery contract](https://github.com/naviapps/configuration-storage/blob/main/docs/RECOVERY.md#portable-recovery-copies)
describes path selection, integrity and reviewed restoration.
`InvalidRecoveryCopy` reports invalid framing or integrity.
`UnsupportedRecoveryVersion` identifies an unsupported copy format.

## Concurrency and native compatibility

Native work runs on the thread pool, serialized per store facade. Other facades
or processes can receive `Busy`. Cancelling before native dispatch prevents I/O.
Once native work starts, its result is delivered even after cancellation. Disposal
rejects queued and new work and retains the native owner for any dispatch already
running. Await outstanding tasks to observe completed writes during shutdown.

`StorageException.Failure` carries the `ErrorCode` and any `RecoveryDirectory`.
That directory contains [raw evidence](https://github.com/naviapps/configuration-storage/blob/main/docs/RECOVERY.md#raw-recovery-limits).
`UnsupportedFormatVersion` identifies an unsupported stored SQLite format.
`UnsupportedAbiVersion` reports a native ABI mismatch before store, save-queue
or recovery-copy work starts. ABI 14 uses JSON metadata and a separate byte
buffer. The binding copies borrowed bytes and supports reflection-disabled JSON.
Invalid native replies produce `InvalidDataException`. Native loader exceptions
retain their standard .NET types.

## Optional client write coalescing

`SnapshotSaveQueue<TSnapshot>` keeps the latest pending immutable client
snapshot. `Enqueue` performs no writes. Explicit `FlushAsync` calls share one
writer, retain current failures for retry and keep obsolete failures from
replacing newer edits. The host chooses flush timing. `State` uses client
revisions independent of on-disk generations and compares by revision fields
and `HasUnsavedChanges`.

## Development and validation

Use the [development guide](https://github.com/naviapps/configuration-storage/blob/main/docs/development.md)
for source fixtures, native builds and Windows package acceptance.
