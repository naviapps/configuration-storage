# Backup and recovery

For store setup and explicit writes, see the [SDK overview](../README.md).
For managed examples, see the [.NET guide](../bindings/dotnet/README.md).

## Backup validation and restore

```swift
let preparation = try await store.prepareBackup(expectedGeneration: saved.generation!)
if let existing = preparation.backup {
  // The host must validate existing.snapshot.payload before authorizing replacement or reuse.
}
let backup = try await store.createBackup(
  expectedGeneration: saved.generation!, token: preparation.token)

let candidate = try await store.readBackup()
// The host must decode and semantically validate candidate.snapshot.payload here.
// Only after that validation and the host's recovery decision, while the selected
// raw state still matches this candidate:
let restored = try await store.restoreBackup(token: candidate.token)
```

`ConfigurationBackup` exposes a snapshot, an opaque `token` and a `modifiedAt` date.
`prepareBackup(expectedGeneration:)` holds this backup, or its absence, together with
a separate rotation token under the root lock. The host validates the held backup's
payload and passes the outer `preparation.token` to `createBackup(expectedGeneration:token:)`.
That commit rechecks raw primary and backup identities, including absence and no-op reuse.
Any changed raw state or stale/cross-purpose token reports `backupChanged`, requiring
fresh preparation and validation. Preparing with a stale expected generation reports
`conflict`. Preparation never rotates a checkpoint or advances an editing baseline.
The nested `preparation.backup.token` authorizes restoration only and cannot authorize rotation.
Backups, candidates and recovery copies expose signed fractional Unix seconds
through `modifiedAtUnixSeconds`, including dates before 1970. The Swift values
use that field for equality and Codable. Use `modifiedAt` for a Foundation date
to display. A file timestamp describes the filesystem and does not indicate
when the host accepted an edit.
The opaque token binds both the verified backup and the current raw selection, including
its marker and SQLite family. `readBackup()` verifies a private copy of the selected
backup against that raw state and rechecks the state before returning the backup and token.
It checks backup integrity and can inspect a backup while the selected database is
damaged. The host validates payload semantics. `restoreBackup(token:)`
rechecks both identities under the root lock. A replaced backup or a newer active write
fails with `.backupChanged`; a stale token cannot overwrite work saved during validation.
After an intentional edit, corruption or other raw selection change, call `readBackup()`
again and validate that candidate before restoring. Do not reuse the earlier creation token
or treat it as a long-lived restore authorization. An unchanged backup retains its snapshot
and timestamp while its token changes when the current raw selection changes.

Restore selects a fresh database and write generation while retaining every previous
generation file. Older writer baselines remain stale. Raw SQLite families are preserved
before automatic recovery where necessary; an error's `recoveryDirectory` identifies raw
evidence, not a verified logical backup. A corrupt, foreign, missing selected or newer-format
store must not be replaced by a seed or an empty snapshot.
Before an initial save, Rust rechecks that the root still has no selected or
retained data and publishes the first selector without replacing an existing entry.
A competing initialization reports `conflict`. Retained data without a valid
selection reports `invalidStoredData` and requires an explicit recovery decision.
Before explicit restoration replaces an existing `active-generation`, Rust retains
its exact raw selector family, including malformed, non-text or oversized bytes.
The retained selector must match the selector bound to the caller's restore
token. A failed capture prevents publication and preserves the primary selection
and databases.
Rust prepares the replacement database privately, then verifies the fresh generation
before selecting it. If a competing write changes that file or adds a sidecar,
restoration stops and retains the competing files for inspection. Failed restoration
only removes a fresh database whose staged bytes still match and whose sidecars
are absent, provided the selector cannot refer to it.
Raw captures use private new files and leave the source bytes and permissions unchanged,
including when the source is read-only.
Captures bind the exact file family, including missing journal/WAL/SHM members.
Repeated failures reuse a content-verified complete capture of the same raw bytes;
changed bytes retain a separate capture. Backup failures capture the backup family,
not a healthy primary. Captures are evidence, never application-validated checkpoints.
Verified reuse creates no staging copy and rewrites no captured files; it remains
available when the recovery directory no longer permits new files. Modified retained
captures are rejected and never overwritten automatically.

### Reconstruct a damaged checkpoint

`createBackup` never silently replaces an invalid checkpoint. After validating the
current primary payload, the host may explicitly reconstruct its checkpoint:

```swift
let token = try await store.prepareBackupRebuild(expectedGeneration: saved.generation!)
let rebuilt = try await store.rebuildBackup(
  expectedGeneration: saved.generation!, token: token)
```

Preparation and publication require the same verified primary generation. The token
also binds the raw primary selection and backup selector/family. Changes require a
fresh review (`backupChanged` for a stale review, including a newer primary write).
Preparation rejects a mismatched primary generation with `conflict`. Rust checks
the selected checkpoint's owner and format in a private copy of the reviewed
database, journal, WAL and SHM. A readable foreign owner or unsupported format
prevents both preparation and publication.
The host owns whether reconstruction is appropriate and how to report its failure.

Reconstruction publishes a verified, synced `Backups/<UUID>.sqlite` and atomically
selects it through `active-backup`. Rust rechecks the reviewed revision after syncing
the fresh database and staged selector, then attempts to publish the selector.
It retains the previous database and all sidecars
without opening them for recovery, moving them, or deleting them. Rust verifies
that its retained copy of a malformed selector matches the selector reviewed for
reconstruction before replacing it. A missing selector with retained versioned
checkpoints never adopts an arbitrary old file. Existing fixed-name
`Backups/snapshot.sqlite` checkpoints remain readable when no versioned selection is
present. Ordinary `.DS_Store`, `desktop.ini` and `Thumbs.db` files leave the legacy
selection and its review tokens intact. Other unexpected entries still prevent adopting
the fixed-name checkpoint without a selector. Reconstruction retains the original
family and desktop metadata. The SQLite envelope is unchanged. Ordinary updates
replace only a verified healthy selected checkpoint and do not accumulate historical
healthy copies; unchanged updates remain no-ops.

`ConfigurationStorageError.Code` carries stable native failures: `invalidConfiguration`,
`storeMissing`, `ownerMismatch`, `invalidStoredData`, `unsupportedFormatVersion`, `conflict`, `busy`,
`notInitialized`, `backupMissing`, `backupChanged`, `invalidRecoveryCopy`,
`unsupportedRecoveryVersion`, `restoreChanged`, `snapshotCandidateMissing`,
`rawRecoveryLimitExceeded` and `ioFailure`. The facade additionally
reports `unsupportedABIVersion` and `invalidNativeResponse` for native contract failures.
Messages contain codes rather than payload contents; the host owns localized explanations.
The native JSON metadata omits an optional recovery location that cannot be represented as Unicode.
It still returns the original error code and retains the raw files; it never substitutes
a lossy path or aborts while encoding that failure.

`unsupportedFormatVersion` identifies an unsupported stored SQLite format;
`unsupportedABIVersion` identifies an incompatible native ABI before the store opens.
.NET uses `UnsupportedFormatVersion` and `UnsupportedAbiVersion` respectively.
Native ABI 14 applies to the store, independent recovery-copy commands and optional save
queue. Store and recovery requests pass payload bytes separately from JSON metadata.
Update native libraries and bindings together. The SQLite and portable recovery-copy
formats remain unchanged.

`storeMissing` is reserved for a root absent during existing-only admission.
It does not represent missing selected data, missing ownership or a missing lock.
Opening an existing store leaves SQLite unopened, so corruption remains visible to the
same load and explicit recovery operations as a regular store handle.

The SQLite schema and `user_version` remain 1, and the stored `generation` column remains
a write UUID. API generation strings are derived without rewriting stored data. Existing
stores, opaque payloads, retained generation files and logical backups require no migration
or rewrite.

## Raw recovery limits

Raw SQLite families and selector fingerprints are limited to 256 MiB per check.
With no usable backup selector, reconstruction binds the selector and only the
managed database names and their sidecars: up to 1,024 members within the same
byte budget. Unrelated entries are left untouched. The check never authorizes a
rebuild using a partial fingerprint. A damaged selector must be captured before
it can be replaced. Exceeding a limit reports `rawRecoveryLimitExceeded`;
the originals stay in place for inspection or manual recovery. The payload limit
is independent.

## Inspect retained snapshots

Use `listSnapshotCandidates(source: .generation)` or `.backup` to inspect only
one managed directory when the other cannot be enumerated. A scoped query
rejects cursors from the other source. Omitting the filter scans both and
returns an error if either scan fails, including a malformed managed directory.
A missing directory contributes no entries.

Restoring selects a new managed file and retains the previous file. Ordinary saves
update the selected file; this is not a history of every save. A lost or malformed
selector never authorizes adopting an arbitrary retained file. The host can instead
inspect owned generation and backup files without depending on the private SQLite layout:

```swift
let page = try await store.listSnapshotCandidates(limit: 64, after: nil)
for reference in page.candidates {
  let candidate = try await store.readSnapshotCandidate(id: reference.id)
  // Decode and validate candidate.snapshot.payload with the host's schema.
  // The host chooses whether to prepare and explicitly confirm a restoration.
}
// Pass page.nextCursor back as `after` to continue, if it is non-nil.
```

Listing returns unverified references from both managed locations. A rebuild
retains the old backup, so it remains inspectable even if the latest backup or
primary is damaged. Each reference and read result has a `source` (`generation`
or `backup`). `isSelected` describes selection within that source and is `nil`
when selection cannot be determined, including when its selector cannot be read.
Candidate data is still verified independently; write and restore checks remain strict. The fixed-name backup from the original
layout can also be inspected. IDs are
opaque file references, not editing-generation tokens or filesystem paths. Unknown
names, links and directories are left untouched and omitted. Each call holds the
root lock. Limits range from 1 through 256; pagination uses ID order, not modification
dates, and `nextCursor` is the last returned ID when more entries remain. Concurrent
file changes across calls require restarting from the first page for a fresh scan.

On macOS, if opening an existing regular lock file for writing is denied, the
store can open it read-only and must still acquire the same exclusive lock.
This does not recreate or modify the lock file or grant permission to write
the store. Contention remains `busy`; an unsupported lock fails the operation.

Reading copies the SQLite main/journal/WAL/SHM family into private temporary files.
SQLite can recover the disposable copy; it never opens or changes the original
family during inspection. The raw family is bounded to 256 MiB and checked before
and after copying/verification; payloads retain the configured byte limit. A source
change fails the inspection. Corruption, foreign ownership and unsupported formats
remain separate errors. `snapshotCandidateMissing` denotes an absent candidate;
`rawRecoveryLimitExceeded` denotes an excessive raw inspection family. Listing one
bad file never validates it or prevents inspecting other managed candidates.
Rollback journal footers naming a super-journal are refused before SQLite opens
either the managed database or an inspection copy, including malformed references.
The store uses single-database transactions; these references can name attached
files outside the managed family. Live operations preserve the raw journal evidence. See the
[SQLite rollback-journal format](https://www.sqlite.org/fileformat2.html#the_rollback_journal).

`ConfigurationSnapshotCandidate` returns initialized immutable snapshot bytes, the
original file's signed modification date and the selection observation. Inspection
does not advance an editing baseline, choose a payload, authorize a write or delete
retained data. Hosts validate these held bytes, call `prepareRestore(payload:)`
before presenting their restore decision, and pass the same bytes and token to
`restoreSnapshot(payload:token:)`. Existing raw-selection conflict checks still apply.

Swift response decoding also rejects inconsistent snapshot nullness, missing keys,
empty initialized generations, empty backup identities and non-finite dates as
`invalidNativeResponse` at the native boundary. Initialized empty payloads, opaque
nonempty identifiers and signed finite dates remain valid. Host schema validation
is independent of these SDK envelope checks.

## Portable recovery copies

A recovery copy can contain an accepted in-memory edit even when its primary store
cannot be written. Its file operations are independent of any store root and select no
application namespace. Hosts retain their complete payload, schema and UI policy.

```swift
try await ConfigurationRecoveryCopy.write(payload: acceptedDraft, to: selectedURL)
let copy = try await ConfigurationRecoveryCopy.read(from: selectedURL)
// Validate copy.payload against the host's schema.
let token = try await store.prepareRestore(payload: validatedPayload)
// Present the host's explicit restore review before committing this token.
let restored = try await store.restoreSnapshot(payload: validatedPayload, token: token)
```

Read/write operations run away from the caller's actor. The path must be absolute
and contain neither parent components nor embedded NUL characters. The parent
directory must exist. Writes stage private bytes, sync and verify them, then
atomically replace an explicitly selected regular file. On Unix, writes resolve the existing parent once
and use that resolved pathname throughout. The final file name is unchanged;
directory spellings ending in a slash or slash-dot are rejected. Reads and writes
reject final symbolic links. On Unix, a named pipe substituted during opening
cannot make the read wait for a writer. The host must protect its live managed
directories when selecting the resolved destination and obtain any replacement
confirmation.
Resource limits default to 16 MiB and cannot exceed the engine's 64 MiB maximum.

The binary copy format is `NVCSCOPY`, a little-endian UInt32 version (1), a
little-endian UInt64 payload length, a 32-byte SHA-256 digest, then exact payload bytes.
The digest hashes `configuration-storage-recovery-copy-v1` followed by a zero byte,
the preceding 20 header bytes, and the payload. Truncation, trailing bytes, corruption
and excessive lengths are rejected. This checksum proves integrity, not authenticity;
no app identity, license, storage owner or payload schema is inferred from a copy.
`modifiedAt` is the opened file's modification time, not an application-edit timestamp.
Dates before the Unix epoch remain valid metadata and retain their negative value.

`prepareRestore(payload:)` binds the host-reviewed immutable bytes to the current raw
selection under the store lock, without advancing any editing baseline. Pass exactly
those bytes and that token to `restoreSnapshot(payload:token:)`. A changed payload,
selected write, marker, or selected raw SQLite family fails with `restoreChanged`.
A damaged current store can be replaced after an explicit review of its raw identity.
Restoration publishes a fresh write generation and retains original generation files;
old writers remain stale. The host must also retain any in-memory edits before changing
its editing session. A source file changing after `read` cannot alter the immutable
payload already reviewed by the host.

`invalidRecoveryCopy` denotes malformed/corrupt copy bytes; `unsupportedRecoveryVersion`
is independent of the SQLite storage format and native ABI. Copy I/O never restores or
saves a primary store. Cancellation before native entry prevents I/O; acknowledged
writes remain successful after late cancellation.
