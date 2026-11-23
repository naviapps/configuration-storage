# NaviApps.ConfigurationStorage

.NET 10 bindings for the Rust `configuration_storage` library. This package
stores opaque byte snapshots. The host owns schemas, semantic validation, migration
interpretation, editing, save timing and UI.

## Install

Use .NET 10 and a `NaviApps.ConfigurationStorage` package for your
process. Use `osx-arm64` or `osx-x64` on macOS 26 or later. Use `win-x64` with
x64 .NET on Windows 11, including when Windows ARM64 runs the x64 process.
Add the supplied package's directory as a local NuGet source and reference its version:

```xml
<PackageReference Include="NaviApps.ConfigurationStorage" Version="PACKAGE_VERSION" />
```

Replace `PACKAGE_VERSION` with the supplied package version. Each package includes
one runtime's native library and retained notices. All variants share the same
package ID and version. Use separate local feeds and NuGet package caches when
restoring multiple variants. Consumers need the package and the matching .NET
runtime.

## Open and save opaque snapshots

```csharp
using NaviApps.ConfigurationStorage;

using var store = ConfigurationStore.CreateLazy(
    new Configuration(absoluteDirectory, "my-app.configuration"));
var current = await store.LoadAsync();
var saved = await store.SaveAsync(current.Generation, opaqueBytes);
var preparation = await store.PrepareBackupAsync(saved.Generation!);
// Inspect preparation.Backup (or its absence) and decide whether to replace it.
var backup = await store.CreateBackupAsync(saved.Generation!, preparation.Token);

var candidate = await store.ReadBackupAsync();
// Validate candidate.Snapshot.Payload using the host's own schema before restoring.
var restored = await store.RestoreBackupAsync(candidate.Token);
```

The directory must be explicit and absolute. Directory and recovery-copy paths
must be representable as UTF-8 without replacing characters. The owner is a
nonempty ASCII identifier containing letters, digits, `.`, `_` or `-`, at most
1024 characters.
The default payload limit is 16 MiB; `MaximumPayloadBytes` accepts 1 through 64 MiB.
Rust validates owner and payload-limit values before any filesystem operation in
both open modes. The binding preserves those values and translates native failures.
Returned snapshots, backups and portable copies are checked against their
requested decoded payload limit.
The binding reads no product environment variables or default product paths.

`CreateLazy` and `CreateLazyExisting` construct facades without I/O. The first
operation opens the store on the thread pool. Cancellation before native admission
or an oversized write leaves the root untouched. After admission fails, a later
operation attempts it again. Use `OpenAsync` or `OpenExistingAsync` to check
admission immediately.

`CreateLazyExisting` and `OpenExistingAsync` require a store already claimed by
the configured owner. They never create a directory or lock, or claim an
unowned root. Only an absent root returns `StoreMissing`; an unclaimed or foreign
root returns `OwnerMismatch`, while malformed storage metadata or a missing lock
in an owned root fails with `InvalidStoredData`. The handle continues to require
an existing lock on later operations.

```csharp
using var existing = await ConfigurationStore.OpenExistingAsync(
    new Configuration(absoluteDirectory, "my-app.configuration"));
var snapshot = await existing.LoadAsync();
```

Opening checks ownership and locking. It does not load or validate a snapshot.
An owned empty store can open successfully and load an uninitialized snapshot.
A corrupt or missing selected snapshot still fails on load.

`Snapshot.Generation` and `Payload` are both null for an uninitialized store.
An initialized empty snapshot has a generation and a non-null empty payload.
Input bytes are captured before `SaveAsync` returns; each `Payload` getter returns
a fresh copy. Generations are opaque strings produced by Rust from the stored UUID,
owner and payload digest. Pass them unchanged; do not parse or compute them in the
host. Use the generation associated with the host's edited snapshot for optimistic
saves and validated backups. Changed payload bytes invalidate an earlier generation
even if the stored UUID is reused: `SaveAsync` and `PrepareBackupAsync` fail with
`Conflict`. Re-read and validate the changed snapshot before saving or backing it
up. A load or backup observation does not alter a writer baseline inside this binding.

`Snapshot` compares its generation and owned payload bytes. Separately read
equal results work as values in `Equals`, equality operators and hash-based
collections. Backup, backup preparation and candidate records include the
snapshot value and their own metadata in comparisons. Candidate pages compare
ordered references and cursor, and recovery copies compare payload, digest and
modification time.

## Backup review and restoration

`PrepareBackupAsync(expectedGeneration)` returns one `BackupPreparation` containing
the exact existing `BackupSnapshot`, or an explicit null `Backup`, and a rotation
`Token` captured in the same operation. The host reviews that backup or its absence
before calling `CreateBackupAsync(expectedGeneration, preparation.Token)`.
Rust rechecks the primary generation and prepared backup identity under its lease,
including before publication and when creation would reuse a same-generation backup.
An intervening backup change, newly created backup, stale primary review or
wrong-purpose token fails with `BackupChanged`; prepare and review again.
The outer preparation token authorizes rotation only. The nested `Backup.Token`
authorizes restoration only; neither substitutes for the other. There is no
unguarded backup-creation overload.

`BackupSnapshot.ModifiedAt` is Unix seconds including its fractional part. Rust
rechecks the token supplied to `RestoreBackupAsync`. The opaque token binds both
the backup and the current raw active selection, including its SQLite family.
A changed backup or newer write during host validation fails with `BackupChanged`.
After either changes, call `ReadBackupAsync` again, semantically validate its opaque
payload again, and restore using that fresh token. Restoration returns a fresh generation and retains older database
files. `StorageException.Failure` carries the Rust `ErrorCode` and optional
`RecoveryDirectory`. A recovery capture contains original database files for host
recovery; it is not a verified logical backup.

## Rebuild a damaged checkpoint

Hosts may explicitly reconstruct an invalid checkpoint after validating the current
primary snapshot:

```csharp
var review = await store.PrepareBackupRebuildAsync(saved.Generation!);
var rebuilt = await store.RebuildBackupAsync(saved.Generation!, review);
```

Preparation checks the primary generation; a mismatch returns `Conflict`. The review
binds the raw primary and backup selections. `RebuildBackupAsync` rejects a stale review
with `BackupChanged` before opening changed SQLite evidence. Readable foreign owners
and unsupported backup formats remain refused. A fresh verified backup is selected
atomically; the previous file and all journals/WAL/SHM remain intact. Ordinary healthy
checkpoint updates do not retain duplicate histories. Repeated identical raw captures
reuse only a complete content-verified copy. Hosts own reconstruction policy and UI.

## Portable recovery copies

`ConfigurationRecoveryCopy` reads and writes an independent portable file. It
requires an explicit absolute file path and carries opaque payload bytes without a
store root, storage-owner identity or host schema. Its immutable `Payload` getter
returns a fresh copy; `Digest` is 64 hexadecimal characters covering the format
header and payload, and `ModifiedAt` is Unix seconds including its fractional part.
The digest detects corruption; it does not authenticate a sender or establish trust.

```csharp
await ConfigurationRecoveryCopy.WriteAsync(absoluteFilePath, opaqueBytes);
var copy = await ConfigurationRecoveryCopy.ReadAsync(absoluteFilePath);
var payload = copy.Payload;
var token = await store.PrepareRestoreAsync(payload);
// Validate payload using the host's schema and obtain any host-required approval.
var restoredCopy = await store.RestoreSnapshotAsync(payload, token);
```

`WriteAsync` returns `Task` without metadata and captures input bytes before
returning. Rust atomically replaces the explicitly selected regular file and
rejects final links. Its parent directory must already exist. The host must protect
its own managed paths when selecting the destination. Both file methods accept
`maximumPayloadBytes` (default 16 MiB,
1 through 64 MiB) and a final optional `CancellationToken`. Read limits also apply
to decoded response bytes. `InvalidRecoveryCopy` reports invalid framing or
integrity; `UnsupportedRecoveryVersion` reports an unsupported portable-file
version, separately from the SQLite format and native ABI.

`PrepareRestoreAsync(payload)` returns an opaque string token binding those caller
bytes and the current raw selection, including a damaged selected SQLite family.
Preparation leaves the selection unchanged. `RestoreSnapshotAsync(payload, token)`
returns a `Snapshot` with a fresh generation, retaining original files; it also
works on an uninitialized store. Rust performs no host policy or semantic decoding.
Changed caller bytes or an intervening selection change fail with `RestoreChanged`.
Prepare again and repeat host validation before retrying. Both methods capture
caller bytes before returning and use the store's configured payload limit.

## Inspect retained snapshots

Pass `source: SnapshotCandidateSource.Generation` or `Backup` to enumerate one
directory. Keep the source when continuing a page; a cursor from the other
source is rejected. The unfiltered query fails if either managed scan fails.

Retained generation and backup files can be inspected with
`ListSnapshotCandidatesAsync(limit: 64, after: null, cancellationToken: default)`
and `ReadSnapshotCandidateAsync(id, cancellationToken: default)`. Listing returns
an immutable `SnapshotCandidatePage` containing `SnapshotCandidateReference` values
and an optional `NextCursor`. Limits are 1 through 256. IDs and cursors are opaque
strings: pass them unchanged, independently of `Snapshot.Generation`; the facade
does not parse GUIDs or accept file paths. Rust validates canonical candidate IDs.
The page includes known selected files from both sources and uses unique IDs in ordinal
order. A non-null cursor equals the last returned ID; pass it as the exclusive
`after` cursor. Each call locks the root independently, so pagination is weakly
consistent under concurrent edits. Restart from the first page to rescan.

Listing is unverified file metadata. It does not open or validate candidate SQLite
contents, select data, or provide per-save history. Normal saves update the current
generation; these APIs expose files retained by explicit restoration or backup rebuild.
Unknown names, links and directories are omitted and left untouched.
`IsSelected` is `bool?` on both references and read candidates: selection is scoped to `Source` (`Generation` or `Backup`), and null means it
cannot be determined, including when its selector cannot be read. Candidate
verification and write or restore authorization remain strict.

```csharp
var page = await store.ListSnapshotCandidatesAsync();
// The host chooses an ID from unverified metadata according to its own policy.
var inspected = await store.ReadSnapshotCandidateAsync(hostChosenCandidateId);
var payload = inspected.Snapshot.Payload!;
// Validate payload using the host's schema before preparing and restoring it.
var review = await store.PrepareRestoreAsync(payload);
// Ask the host for confirmation with this review token held.
var restoredCandidate = await store.RestoreSnapshotAsync(payload, review);
```

`SnapshotCandidate` contains `Id`, `Source`, an initialized `Snapshot`, `ModifiedAt` (finite
signed Unix seconds), and nullable `IsSelected`. Payload getters return fresh
copies. Rust reads through a private copy of the SQLite family, including sidecars,
without opening the original SQLite database. Storage integrity checks do not
replace host semantic validation. Reading does not change selection or advance a
writer baseline; restoration uses the existing payload/token APIs after host
validation. The SDK adds no candidate choosing, restore/delete UI, automatic
retention, or deletion policy.

Raw inspection is capped at **256 MiB total for the SQLite family**, independently
of the configured `MaximumPayloadBytes` limit on decoded payloads. An oversized raw
family fails with `RawRecoveryLimitExceeded` without modifying originals; a
removed or missing candidate reports `SnapshotCandidateMissing`. Concurrent changes
may instead report `Busy`; invalid IDs/cursors or limits report
`InvalidConfiguration`. Foreign-owner, unsupported-format and corrupt candidates
use the existing storage failures. These failures leave original source bytes
intact. Candidate metadata alone never authorizes restoration.

## Concurrency and native compatibility

`OpenAsync`, `OpenExistingAsync`, deferred admission and every native dispatch run
on the thread pool. One facade serializes its store operations, including
candidate listing/reading, preparation and snapshot
restore. Standalone recovery-copy file methods require no store handle. Other facades or processes may
receive `Busy` from Rust's exclusive root lock. Cancellation applies only before
native work starts. Once native work
has started, its success or failure is delivered even if the token is cancelled.
Disposal rejects queued and new work. A deferred native open already in progress
may finish; disposal closes that handle before dispatching the operation. A dispatch
already running retains its native owner through result consumption. Await
outstanding tasks if the host needs to observe completed writes during shutdown.

Backup, candidate and recovery-copy modification times are signed Unix seconds,
including fractional dates before 1970. Valid payloads remain readable regardless of whether
their modification date precedes the epoch; the date never authorizes restoration.
Both explicit restore operations retain the original primary selector's raw bytes
before replacing it, including malformed selectors. Capture failure leaves the
selection and original databases intact.

This binding requires native ABI 13, which uses owned store/result handles and
borrowed result views. The binding copies each view before releasing its result.
An incompatible ABI produces
`UnsupportedAbiVersion` before opening a store, creating a save queue or invoking
the standalone recovery-copy symbol. An unsupported stored SQLite format produces
`UnsupportedFormatVersion` from Rust's
`unsupportedFormatVersion` failure JSON. Update native libraries and bindings
together. SQLite format version 1 is unchanged; existing files remain readable
without rewriting or data migration. Update all writers to this SDK before using the
new backup selector; prior native runtimes do not understand it.
Invalid native replies produce `InvalidDataException`.
Native loader exceptions retain their standard .NET types.

## Development and validation

After [developer setup](../../docs/development.md#setup-and-source-layout), run
the managed fixture from the SDK repository root using the pinned .NET SDK:

```sh
mise exec -- make test-managed
```

Managed acceptance checks the binding contract with fake-native responses,
including ownership, framing, limits, ABI rejection, cancellation, serialization
and disposal. It requires no native artifact.

For macOS native C/.NET acceptance and a fresh local NuGet consumer, run
`mise exec -- make test-bindings`. It uses the pinned SDK Tooling CLI to verify
and consume the existing `Artifacts/CConfigurationStorage.xcframework`.
The source fixture passes `ConfigurationStorageNativeLibrary` to MSBuild to
copy the selected library to its output. The NuGet consumer loads the packaged
native asset through standard .NET resolution.
`mise exec -- make test-c` selects the C subset after native verification. Runtime cases cover opaque bytes, generation conflicts, reviewed
backup/restore tokens, retained originals and snapshot candidate inspection.

### Windows x64 package acceptance

Use an owned Windows 11 environment with native ARM64 or x64 Python and Rust.
Prepare the Rust version pinned in `rust-toolchain.toml`, its
`x86_64-pc-windows-msvc` standard library, locked Cargo dependencies and the
Visual Studio C++ toolchain. The toolchain must support an x64 target from
the native host architecture. On ARM64 this requires ARM64-hosted MSVC with
x64 target components and Windows x64 emulation to execute the resulting tests.
Set `DOTNET_BIN` to the absolute path of the x64 .NET SDK 10.0.403 executable.

From the SDK root, use the SDK Tooling revision pinned in
[`mise.toml`](../../mise.toml) to run the Windows acceptance fixture.

```powershell
$env:DOTNET_BIN = 'C:\Program Files\dotnet\x64\dotnet.exe'
mise run sdk-tooling -- python -m distribution.verify_windows
```

Use the installed x64 executable's path on your machine. The driver builds the
x64 Rust DLL and bundled SQLite with the static CRT, verifies the native exports
and retained notices, and runs Rust checks, C ABI and standalone C fixtures,
managed tests and an installed NuGet consumer. That consumer restores the `win-x64` package
from an isolated local feed. The verified package and its hash receipt go to
`build/windows/win-x64`, or to `win-x64` under `VM_RUNNER_ARTIFACTS_DIR` when that
variable is set. See the [development guide](https://github.com/naviapps/configuration-storage/blob/main/docs/development.md)
for CI prerequisites and notice policy.

## Optional client write coalescing

`SnapshotSaveQueue<TSnapshot>` optionally coalesces immutable client snapshots.
`Enqueue` performs no writes; explicitly call `FlushAsync` at host-selected times.
Concurrent flushes share one writer. Failed current snapshots remain pending for
explicit retry; older failures cannot replace newer edits. The queue owns no timer,
retry loop, storage path, payload schema, permission or failure UI. Its `State` uses
client revisions, independently of the store's on-disk generation tokens.
State observations compare by the revision fields and `HasUnsavedChanges`.
Repeated reads without a transition compare equal and hash consistently.
