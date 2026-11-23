# ConfigurationStorage

Opaque, transactional configuration snapshots backed by a Rust engine and a synchronous C ABI.
The Swift product and module are `ConfigurationStorage`; its native framework is
`CConfigurationStorage.xcframework`. Swift requires Swift 6.3 and macOS 26 or later.
The Swift and .NET bindings require native ABI 13.

## Documentation

- [Swift API guide](https://github.com/naviapps/configuration-storage/blob/main/Sources/ConfigurationStorage/ConfigurationStorage.docc/ConfigurationStorage.md):
  store admission, write generations, recovery and client write coalescing.
- [Backup and recovery](https://github.com/naviapps/configuration-storage/blob/main/docs/RECOVERY.md): checkpoint review, retained snapshots,
  raw recovery evidence and the portable-copy format.
- [.NET API guide](https://github.com/naviapps/configuration-storage/blob/main/bindings/dotnet/README.md): managed ownership, cancellation and packaging.
- [Development](https://github.com/naviapps/configuration-storage/blob/main/docs/development.md): setup, source layout, check commands,
  native artifacts and retained notices.

## Install with Swift Package Manager

In an ordinary Swift executable or app package, add the repository at an immutable
published revision and select the production product. Replace
`<FULL_PUBLISHED_COMMIT_SHA>` below with the complete 40-character commit SHA of
the published SDK revision you have reviewed; it is a placeholder, not a branch or tag.

```swift
// swift-tools-version: 6.3
import PackageDescription

let package = Package(
  name: "StorageClient",
  platforms: [.macOS(.v26)],
  dependencies: [
    .package(
      url: "https://github.com/naviapps/configuration-storage.git",
      revision: "<FULL_PUBLISHED_COMMIT_SHA>"),
  ],
  targets: [
    .executableTarget(
      name: "StorageClient",
      dependencies: [
        .product(name: "ConfigurationStorage", package: "configuration-storage"),
      ]),
  ]
)
```

Place your Swift source in `Sources/StorageClient/`, import `ConfigurationStorage`,
and run `swift build` from your package root with Swift 6.3 or newer on macOS 26 or later.
SwiftPM consumes the tracked native framework without developer tooling, a Rust
installation or a native rebuild.

## Install from Rust or C

A Rust package can depend on the public storage crate at the same immutable Git
revision. Replace `<FULL_PUBLISHED_COMMIT_SHA>` with the complete published SHA:

```toml
[dependencies]
configuration-storage = { git = "https://github.com/naviapps/configuration-storage.git", rev = "<FULL_PUBLISHED_COMMIT_SHA>" }
```

The Rust crate builds bundled SQLite and needs a C toolchain for its target.

A macOS C program installs `CConfigurationStorage.framework` from the universal
slice of `Artifacts/CConfigurationStorage.xcframework` with its retained notices.
Include `<CConfigurationStorage/configuration_storage.h>` and link
`-framework CConfigurationStorage` with `-F` and a runtime search path pointing to
the installed framework's parent directory. The C client does not need Rust or
the SDK source checkout. Public C types use the full `ConfigurationStorage` prefix,
including `ConfigurationStorageStore` and `ConfigurationStorageResultView`.

## Install from .NET

Obtain the `NaviApps.ConfigurationStorage` package for your .NET 10
process. Use `osx-arm64` or `osx-x64` on macOS 26 or later and `win-x64` with
x64 .NET on Windows 11, including x64 processes on Windows ARM64.
Add the supplied package's directory as a local NuGet source and reference its version:

```xml
<PackageReference Include="NaviApps.ConfigurationStorage" Version="PACKAGE_VERSION" />
```

Each package carries one native runtime. All variants share the same ID and
version. Use separate local feeds and NuGet package caches when restoring
multiple variants. See the [.NET API guide](https://github.com/naviapps/configuration-storage/blob/main/bindings/dotnet/README.md).

## Responsibility

Rust owns file ownership, transactional writes, explicit generation conflicts, storage-format
and integrity checks, logical backups, backup identity rechecks and non-destructive restore.
Swift owns actor isolation, the native handle lifetime and conversion between owned native
results and immutable Swift values. Payloads cross the JSON ABI as base64 through `Data.Codable`;
they are never interpreted as application JSON, text or model objects.

The host chooses the directory and owner, payload schema, semantic validation, migrations,
editing baseline, save timing, retry/merge decisions, backup timing and recovery confirmation.
There is no application import, namespace inference, Defaults access, scheduler, automatic
save, migration policy, consent policy or UI in this package.

## Explicit writes

```swift
import ConfigurationStorage

let store = ConfigurationStore(
  configuration: .init(
    directory: storageDirectory, // Host-selected file URL.
    owner: "com.example.application",
    maximumPayloadBytes: 16 * 1024 * 1024))

let editingBase = try await store.load()
// The host decodes and validates editingBase.payload, then produces encodedDraft.
let saved = try await store.save(
  expectedGeneration: editingBase.generation,
  payload: encodedDraft)
```

Construction performs no I/O; the first actor operation validates the configuration
and opens the store. Use an absolute local file URL without `..` components.
The managed root, its subdirectories and files reject symbolic links. The payload
limit defaults to 16 MiB and accepts 1 byte through 64 MiB.
The .NET equivalents are `ConfigurationStore.CreateLazy` and
`ConfigurationStore.CreateLazyExisting`.

To require an existing owned store, use `ConfigurationStore(openingExisting: configuration)`.
To check admission immediately, await `ConfigurationStore.openExisting(configuration:)`.
That method corresponds to Rust `ConfigurationStore::open_existing`, C
`configuration_storage_store_open_existing` and .NET `ConfigurationStore.OpenExistingAsync`.
Only an absent root returns `storeMissing`; an unclaimed or foreign root returns
`ownerMismatch`. This mode never creates a root or lock. Opening checks ownership
and locking; snapshot validation happens during subsequent operations.
To open or claim a store immediately, await `ConfigurationStore.open(configuration:)`.

`ConfigurationSnapshot.generation` and `payload` are both `nil` for an uninitialized
store. An initialized empty payload is `Data()` with a generation. Keep the generation
associated with the snapshot being edited and pass it unchanged to `save`; `nil`
only initializes an uninitialized store. A stale generation fails with `.conflict`.
A later `load()` never advances the host's editing baseline. Saving unchanged bytes
with the current generation retains it.

Unreadable, foreign, missing selected or newer-format data must not be replaced
with a seed. Retained generations, backups and raw recovery evidence also prevent
an apparently empty selection from being treated as a fresh store.

Actors serialize their operations; other instances or processes can encounter `.busy`.
The host decides how to handle contention and conflicts. Cancellation before native
entry prevents I/O. Once synchronous native work begins, it runs to completion and
delivers its result, including a confirmed write after late cancellation.

Backup rotation requires `prepareBackup`, host validation of its held backup or
absence, and `createBackup` with the preparation token. Restore uses a separate
backup token. The [backup and recovery guide](https://github.com/naviapps/configuration-storage/blob/main/docs/RECOVERY.md)
covers review, damaged checkpoints, retained snapshots and portable copies;
the [Swift API guide](https://github.com/naviapps/configuration-storage/blob/main/Sources/ConfigurationStorage/ConfigurationStorage.docc/ConfigurationStorage.md)
details admission, paths and generation/token behavior.

## Optional client write coalescing

Swift `SnapshotSaveQueue<Snapshot>` and .NET `SnapshotSaveQueue<TSnapshot>` coalesce
immutable pending snapshots without starting a scheduler. Enqueue never writes;
explicit flushes share one writer, retain current failures for retry and prevent
obsolete failures from restoring old edits. Hosts choose debounce, flush and retry
timing. Construct Swift queues with `try SnapshotSaveQueue(commit:)` and propagate
initialization failure before accepting edits. See the language API guides for details.

## Test fixtures

`ConfigurationStorageTestSupport` is an optional Swift product for explicitly owned
temporary stores. `ConfigurationStorageFixture` inspects opaque payloads and
installs deliberate corruption or write failures without exposing the runtime
schema to host tests. Import it only in test targets. Construction performs no I/O
and never cleans up a directory; the caller must own and retire its fixture.
Product payload schemas, previous product SQL fixtures and migration tests remain
in the host application. Ordinary consumers need only `ConfigurationStorage`.

## Development

Run `mise exec -- make check` for source lint, Rust Clippy and workspace tests,
.NET binding tests, strict Swift tests and the standalone SwiftPM consumer. The
Swift checks use the tracked native framework. See the
[development guide](https://github.com/naviapps/configuration-storage/blob/main/docs/development.md)
for setup, focused checks and maintainer distribution validation.

## License

This project is available under the [MIT License](LICENSE). Third-party components
retain their original notices in [ThirdParty](ThirdParty).
