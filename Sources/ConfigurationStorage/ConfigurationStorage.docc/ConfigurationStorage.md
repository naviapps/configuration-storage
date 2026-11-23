# `ConfigurationStorage`

Persist opaque bytes with explicit write generations and non-destructive recovery.

## Overview

`ConfigurationStore` is an actor initialized with an immutable
`ConfigurationStore/Configuration`. Initialization performs no I/O. The actor lazily
opens and owns its Rust-backed handle inside its operations, transfers result ownership to
response `Data`, and destroys the handle after its final dispatch. The response storage releases
each native result when it no longer needs its bytes.
Await ``ConfigurationStore/open(configuration:)`` to open or claim a root immediately and
receive admission errors before performing any snapshot operation.
The Swift and .NET bindings require native ABI 13.

The engine owns transactions, storage ownership and format, integrity, generation conflicts,
backup identity and retained generations. The host owns payload schemas, semantic validation,
migration meaning, editing, save/backup timing, retry policy and UI. No namespace or global
application settings are selected by this package.

The host supplies an absolute root without `..` components. At native open, existing parent
aliases are resolved before missing directories are privately created. The handle retains
the concrete root even if a parent alias is later retargeted. The managed root itself,
subdirectories and files reject symbolic links, and reopening still verifies ownership.
Swift store and recovery-copy URLs accept only an empty host or case-insensitive
`localhost`. User information, ports, queries and fragments fail before native
I/O. Use `URL(fileURLWithPath:)` for names that contain literal question marks or
hashes. Mounted volumes use their explicit local file paths. Encoded slashes
and NUL characters are invalid. A trailing slash is kept as part of the caller's
path and cannot select a regular recovery file.

Use `ConfigurationStore(openingExisting: configuration)` when missing storage must not
be created or claimed. Await ``ConfigurationStore/openExisting(configuration:)`` to check the
same admission rules immediately. Admission checks the existing owner and lock without opening
SQLite. Admission reports ``ConfigurationStorageError/Code/storeMissing``
only when the root does not exist. An unclaimed or foreign root reports
``ConfigurationStorageError/Code/ownerMismatch``; an invalid owner or lock remains an
error. The actor never creates a missing root lock on later operations. An admitted
store can still be uninitialized; its operations keep their usual read, recovery and
write behavior.

## Preserve the editing baseline

`ConfigurationSnapshot` distinguishes an uninitialized `nil` payload/generation from
initialized empty `Data`. `save(expectedGeneration:payload:)` requires the generation of
the host's editing base, or explicit `nil` for initialization. An observation from `load()`
does not advance or replace that baseline. A stale write fails with
`ConfigurationStorageError/Code/conflict` even after the caller observes a newer snapshot.
Payloads use base64 on the wire through `Data`'s Codable support, retaining arbitrary bytes.
Oversized save and restore requests fail before encoding or lazily opening the store.

An initialized generation is an opaque, content-bound string derived by Rust from the
stored write UUID, owner and verified payload digest. A different payload and matching valid
digest change the API generation even when the stored write UUID is unchanged. Pass the
returned string back unchanged; never parse, synthesize or reconstruct it from raw storage.
Both `save(expectedGeneration:payload:)` and `prepareBackup(expectedGeneration:)` reject a
stale generation with `ConfigurationStorageError/Code/conflict` in this case. Saving an
unchanged payload with the current generation retains that generation.

Rust retains complete stored-envelope validation of format, schema, ownership, row count,
payload size and digest, SQLite integrity checks and backup-copy verification. Writes still
verify the stored result inside the transaction before acknowledgement.

## Validate before restore

``ConfigurationStore/prepareBackup(expectedGeneration:)`` holds the selected backup, or
its absence, and a rotation token in one native observation under the root lock. The returned
``ConfigurationBackupPreparation/backup`` is an integrity-checked immutable snapshot;
the host must validate its payload semantics before authorizing replacement or reuse.
Preparation does not modify the checkpoint or advance an editing baseline.

```swift
let preparation = try await store.prepareBackup(expectedGeneration: expectedGeneration)
if let existing = preparation.backup {
    // Validate existing.snapshot.payload against the host schema before allowing rotation.
}
let checkpoint = try await store.createBackup(
    expectedGeneration: expectedGeneration, token: preparation.token)
```

``ConfigurationStore/createBackup(expectedGeneration:token:)`` rechecks the exact raw primary
and backup identities captured during preparation, including an observed absence. A changed
selection, file family or primary write fails with
``ConfigurationStorageError/Code/backupChanged`` before rotation, even when the resulting
checkpoint would otherwise be a no-op. A stale or cross-purpose token also fails with that code.
The outer ``ConfigurationBackupPreparation/token`` authorizes this rotation only; the nested
``ConfigurationBackup/token`` is a restore token and cannot authorize rotation.

First publication selects a fresh UUID file; subsequent checkpoints atomically replace the
verified healthy selected backup. An unchanged generation remains a no-op after the identity
recheck. Ordinary checkpoints do not retain a history of healthy backups and refuse invalid
backup selections or families. `readBackup()` returns a `ConfigurationBackup` whose payload
the host must semantically validate.
Its opaque token binds both the backup and the current raw selection, including the marker
and SQLite family. Only after host validation pass that token to `restoreBackup(token:)`.
The engine rechecks both identities: a replaced backup or newer active write fails with
`ConfigurationStorageError/Code/backupChanged`. After intentional edits, corruption or
other raw selection changes, call `readBackup()` again and validate the new candidate before
restoring. Tokens are not long-lived restore authorization. An unchanged backup can retain
its snapshot and timestamp while the token changes with the raw selection.
Restore selects a new file and write generation, retaining previous files and invalidating
older writers.

## Inspect retained snapshot candidates

``ConfigurationStore/listSnapshotCandidates(limit:after:source:)`` lists unverified references to
managed generation and backup files. Explicit restoration retains old generations and
backup rebuild retains the previous checkpoint, including fixed-name legacy backups.
Both candidate models expose a `source` (generation or backup). Ordinary saves update
the selected generation in place; this API does not provide per-save history.

Pass `source: .generation` or `.backup` to enumerate only that managed directory.
Continue with a cursor from the same source. The unfiltered query fails when
either managed directory cannot be enumerated; a missing directory is empty.

Listing defaults to 64 entries with no cursor and accepts limits from 1 through 256. Each
``ConfigurationSnapshotCandidatePage`` contains unique references sorted by candidate ID.
If ``ConfigurationSnapshotCandidatePage/nextCursor`` is non-`nil`, it equals the last returned
ID; pass it unchanged as `after` for an exclusive continuation. Calls hold the root lock
individually. Pagination is weakly consistent across calls, so restart without a cursor to
rescan concurrent edits.

Candidate IDs are opaque managed file identities, distinct from content-bound snapshot
generation strings. Pass them unchanged; they are not file paths. Listing includes only real
canonical UUID `.sqlite` files in the owned `Generations` and `Backups` directories,
plus the fixed-name legacy backup. Unknown names, symbolic
links and directories are omitted and left untouched. An indeterminate selection for that source, including an unreadable selector,
makes ``ConfigurationSnapshotCandidateReference/isSelected`` and
``ConfigurationSnapshotCandidate/isSelected`` `nil`, meaning selection is unknown. Neither
listing nor reading repairs the selector or adopts any candidate automatically.

``ConfigurationStore/readSnapshotCandidate(id:)`` copies the candidate's SQLite family,
including sidecars, into private storage before opening SQLite. It verifies integrity,
ownership, format and configured payload limits without opening or modifying the original
SQLite files. The returned ``ConfigurationSnapshotCandidate`` holds an initialized snapshot
and immutable verified bytes; the host still owns semantic validation. File modification time
preserves finite signed Unix seconds, including fractional dates before 1970, and does not
identify when the host accepted an edit.

```swift
let page = try await store.listSnapshotCandidates()
// After the host chooses an ID from the reviewed references:
let candidate = try await store.readSnapshotCandidate(id: hostChosenCandidateID)
if let heldBytes = candidate.snapshot.payload {
    // Validate these exact bytes against the host schema before preparing the token.
    let token = try await store.prepareRestore(payload: heldBytes)
    // Obtain the host's restore decision with this token held, and preserve unsaved edits.
    let restored = try await store.restoreSnapshot(payload: heldBytes, token: token)
}
```

The example requires host validation before preparation and host confirmation afterward;
listing order and `isSelected` do not choose an acceptable payload. Reading never advances a writer's
editing baseline. Later source edits cannot change held result bytes. Existing snapshot restore
preparation binds those bytes and the current raw selection; a changed selection requires a
new token and review. Restoration selects a fresh generation and retains the originals.

An absent candidate reports ``ConfigurationStorageError/Code/snapshotCandidateMissing``.
A candidate changed or removed during inspection can report that error or
``ConfigurationStorageError/Code/busy``. Foreign, newer-format and corrupt files report existing
ownership, format or stored-data errors. Inspection bounds raw SQLite family data to 256 MiB
and performs bounded metadata checks, copying and fingerprinting; excessive families report
``ConfigurationStorageError/Code/rawRecoveryLimitExceeded`` without changing originals.
The configured payload limit still applies independently. The package adds no automatic
selection, retention cleanup, history UI or direct candidate-restore operation.

## Explicitly rebuild an invalid backup

``ConfigurationStore/prepareBackupRebuild(expectedGeneration:)`` verifies the current primary
against the host's explicit generation and returns an opaque review token. Preparation does
not publish a new backup or advance an editing baseline. A stale expected generation fails with
``ConfigurationStorageError/Code/conflict``. The host explicitly chooses reconstruction after
reviewing the failure and preserving any unsaved edits, then supplies the same generation and
token to ``ConfigurationStore/rebuildBackup(expectedGeneration:token:)``:

```swift
let token = try await store.prepareBackupRebuild(expectedGeneration: expectedGeneration)
// Obtain the host's explicit confirmation to reconstruct the backup.
let backup = try await store.rebuildBackup(expectedGeneration: expectedGeneration, token: token)
```

Rust holds the root lock and binds the token to the complete raw primary selector and SQLite
family, plus the raw backup selector and family. Rebuild checks this identity before opening
the primary. Any stale token, including after a newer primary write or changed sidecar, fails
with ``ConfigurationStorageError/Code/backupChanged`` and requires fresh preparation and host
review. Changing the supplied generation cannot reuse an old review token. Tokens authorize
this reconstruction only; they do not authorize restoration or a later save.

An explicit rebuild creates a fresh verified `Backups/<UUID>.sqlite` and selects it through
the root's `active-backup` marker, retaining the prior backup family without modifying its
files. It returns a ``ConfigurationBackup`` and leaves the primary generation unchanged.
Its returned backup token can be used for restoration only after host payload validation.
Known readable backups belonging to a foreign owner or unsupported format remain refused with
``ConfigurationStorageError/Code/ownerMismatch`` or
``ConfigurationStorageError/Code/unsupportedFormatVersion``. This opt-in operation does not
relax ordinary checkpoint creation.

The legacy `Backups/snapshot.sqlite` is readable only when `active-backup` is absent and no
UUID checkpoint files exist. A missing marker alongside UUID checkpoints, or any invalid
marker, is an error; retained UUID files are never adopted automatically.

A failure's recovery directory contains raw SQLite evidence rather than a verified backup.
Unreadable, foreign, missing selected and newer-format stores are errors, not permission
to seed or silently migrate application data.

Explicit restoration retains an existing raw `active-generation` selector before
replacing it, including malformed, non-text and oversized bytes. This keeps the old
selection evidence alongside the untouched original databases. A failed capture
prevents publication. An absent selector needs no file capture.

`ConfigurationStorageError/Code/unsupportedFormatVersion` means the stored SQLite
format version is unsupported. `ConfigurationStorageError/Code/unsupportedABIVersion`
means the native ABI is incompatible, before a store is opened. The Swift and .NET
bindings require native ABI 13, which requires reviewed identity tokens for backup rotation.
Update native libraries and bindings together.

The SQLite schema and `user_version` remain 1, and the stored `generation` column remains
a write UUID. API generation strings are derived without rewriting stored data. Existing
stores, opaque payloads, retained generation files and logical backups require no migration
or rewrite.

## Recover an accepted draft independently

``ConfigurationRecoveryCopy`` reads and writes portable integrity-protected files
without opening any store. Hosts keep their complete accepted payload and choose a
confirmed destination outside live managed storage. The parent directory must exist.
On Unix, writes resolve its aliases once and keep the final file name unchanged.
Writing atomically replaces a regular file only after private staged bytes sync and
verify. Symbolic-link files, corruption, truncation, trailing bytes and excessive
payloads are rejected. The checksum is not authentication or app/schema validation.

```swift
try await ConfigurationRecoveryCopy.write(payload: acceptedDraft, to: selectedURL)
let copy = try await ConfigurationRecoveryCopy.read(from: selectedURL)
// Validate copy.payload against the host schema and explicitly confirm restoration.
let token = try await store.prepareRestore(payload: validatedPayload)
let restored = try await store.restoreSnapshot(payload: validatedPayload, token: token)
```

The reviewed immutable payload and current raw selection are bound together. Changes
to either cause ``ConfigurationStorageError/Code/restoreChanged`` and require review
again. This can recover a damaged selected generation without modifying its original
bytes. Restore publishes a fresh generation, retaining previous generation files;
previous writer baselines stay stale. Hosts must preserve their in-memory edits before
replacing an editing session. Copy reads alone never authorize a restore or advance a
writer's baseline.

Use `modifiedAtUnixSeconds` on backups, candidates and recovery copies for the engine's
signed fractional Unix seconds, including times before 1970. Equality and Codable use
this value. Use `modifiedAt` for a Foundation date suitable for display. File timestamps
describe the filesystem and are independent of application edit acceptance, payload
integrity and write permission.

The copy format is versioned independently of SQLite and the ABI. Resource limits
default to 16 MiB with a 64 MiB engine maximum. Store and recovery-copy replies
must fit the requested limit after decoding. See the
[backup and recovery guide](https://github.com/naviapps/configuration-storage/blob/main/docs/RECOVERY.md#portable-recovery-copies)
for the binary format and digest contract. Independent copy calls use a concurrent executor and retain
confirmed results after late cancellation, just like store writes.

## Cancellation and scheduling

Explicit open factories run away from the caller's actor. Later operations enter the store
actor. Each native operation is synchronous and cannot be interrupted. Cancellation before native entry
throws `CancellationError`. Cancellation after entry cannot undo a write; confirmed results
remain confirmed. The store has no automatic saving, backup scheduler, host callbacks or UI.

## Optional client write coalescing

`SnapshotSaveQueue` retains immutable host snapshots, while Rust owns revision
issuance, in-flight ownership and current-versus-superseded failure handling.
`enqueue` performs no I/O and replaces only the latest pending snapshot. Explicit
`flush()` drains it, sharing one writer across concurrent callers. A current
failure remains pending for explicit retry. An older failed write cannot restore
obsolete edits over a newer pending snapshot. Cancelling a flush caller does not
cancel or misreport an owned commit.

Construct the Swift queue with `try SnapshotSaveQueue(commit:)`. Initialization
throws ``ConfigurationStorageError/Code/unsupportedABIVersion`` before native queue
creation on an ABI mismatch, or
``ConfigurationStorageError/Code/invalidNativeResponse`` for a missing native handle.
Propagate this failure before accepting edits. Queue construction enforces the same
native ABI 13 requirement as the store and independent recovery-copy APIs.

`SnapshotSaveState` is a coherent observation of client revisions, independent
of the store's opaque generation strings. State observations compare by the
revision fields and `hasUnsavedChanges`. The host chooses timers, debounce,
when to flush/retry, backup policy and failure UI. The queue does not change the
store's scheduler-free default or select any of those policies. Revision
exhaustion rejects a new enqueue without changing retained work; the host must
retain and report the rejected unsaved edit.

## Topics

### Storage

- ``ConfigurationStore``
- ``ConfigurationStore/Configuration``
- ``ConfigurationStore/open(configuration:)``
- ``ConfigurationStore/openExisting(configuration:)``
- ``ConfigurationSnapshot``

### Recovery and failures

- ``ConfigurationBackup``
- ``ConfigurationBackupPreparation``
- ``ConfigurationStore/prepareBackup(expectedGeneration:)``
- ``ConfigurationStore/createBackup(expectedGeneration:token:)``
- ``ConfigurationSnapshotCandidateReference``
- ``ConfigurationSnapshotCandidateSource``
- ``ConfigurationSnapshotCandidatePage``
- ``ConfigurationSnapshotCandidate``
- ``ConfigurationStore/listSnapshotCandidates(limit:after:source:)``
- ``ConfigurationStore/readSnapshotCandidate(id:)``
- ``ConfigurationStore/prepareBackupRebuild(expectedGeneration:)``
- ``ConfigurationStore/rebuildBackup(expectedGeneration:token:)``
- ``ConfigurationRecoveryCopy``
- ``ConfigurationStore/prepareRestore(payload:)``
- ``ConfigurationStore/restoreSnapshot(payload:token:)``
- ``ConfigurationStorageError``
- ``ConfigurationStorageError/Code``

### Client write coalescing

- ``SnapshotSaveQueue``
- ``SnapshotSaveState``
- ``SnapshotSaveQueueError``
