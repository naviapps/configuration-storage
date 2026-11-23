# Development

Run commands from the repository root. The tests use owned temporary stores.
For SDK installation and explicit writes, see the [overview](../README.md).

## Setup and source layout

Use Git, mise and a full Xcode installation providing Swift 6.3 or newer, SwiftPM,
and Apple build tools on macOS 26 or later. `mise.toml` selects developer Python
and static tools. `global.json` pins .NET. `rust-toolchain.toml` selects Rust 1.99.1,
rustfmt and Clippy. Review those manifests, then prepare the pinned tools before running
checks:

```sh
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
mise trust mise.toml
mise install
mise exec -- cargo fetch --locked
mise exec -- make check
```

Adjust `DEVELOPER_DIR` to the installed Xcode; no global Xcode selection is needed.
The fetch prepares the workspace dependency cache for the locked offline Rust checks.
Contributor checks use the pinned tools and the tracked framework. The shared
commands fetch the public SDK Tooling revision in [mise.toml](../mise.toml)
on first use. SwiftPM and NuGet installation use the distributed SDK.

Swift sources and tests live in `Sources/` and `Tests/`. See
[Swift usage](../README.md#explicit-writes) for the public API. Rust crates live in
`crates/`, with C ABI tests in `crates/ffi/tests/c/`. .NET projects use
`bindings/dotnet/src/` and `bindings/dotnet/tests/`. Packaging checks are in
`distribution/` and independent consumers are in `fixtures/consumers/`. Cross-language guides are
in `docs/`. Tracked native frameworks and receipts are in `Artifacts/`.

## Choose a check

`make help` lists the available targets; the default `make` runs `make check`.
Run Make targets through `mise exec --` so their recipes use the pinned tools.

| Command                         | Coverage                                                                                                | Executes tests |
| ------------------------------- | ------------------------------------------------------------------------------------------------------- | -------------- |
| `make lint`                     | Owned source format/lint, workflow checks and managed whitespace                                        | No             |
| `make lint-rust`                | Rust workspace Clippy, locked/offline                                                                   | No             |
| `make native-verify`            | Source/artifact receipt, notices, exports and platform verification                                     | No             |
| `make check-rust`               | Rust workspace library and test compilation, locked/offline                                             | No             |
| `make check-swift`              | Strict Swift library and test compilation against the tracked framework                                 | No             |
| `make check-fast`               | Owned source lint and both Rust/Swift compilation checks                                                | No             |
| `make check`                    | Source lint, Rust Clippy and tests, .NET binding tests, Swift tests and the standalone SwiftPM consumer | Yes            |
| `make check-distribution`       | Distribution fixture tests, native verification, C/.NET/NuGet checks and installed Rust/C clients       | Yes            |
| `make test-rust`                | Rust workspace tests and doctests, locked/offline                                                       | Yes            |
| `make test-managed`             | .NET binding tests with fake-native responses                                                           | Yes            |
| `make test`                     | Strict Swift tests against the tracked framework                                                        | Yes            |
| `make test-consumer`            | Standalone SwiftPM public-API consumer                                                                  | Yes            |
| `make test-c`                   | Native verification and disposable-store C ABI consumer                                                 | Yes            |
| `make test-bindings`            | Native verification, C/.NET tests and clean local NuGet consumer                                        | Yes            |
| `make test-distribution-tools`  | SDK build/fixture policy tests                                                                          | Yes            |
| `make test-installed-consumers` | Independent Git Rust and installed C clients using fresh consumer directories                           | Yes            |
| `make acceptance`               | `check` and `check-distribution`                                                                        | Yes            |

Use the compilation checks while editing and the relevant test command before
handoff. Compilation success does not validate storage behavior, ownership,
recovery or cancellation. `make acceptance` is the full local macOS acceptance
path against the existing source-matched framework. It requires [SDK Tooling](#maintainer-distribution-checks), the pinned tools,
provisioned offline Cargo dependencies for the workspace, Xcode and .NET 10.
The independent Rust client also needs access to the public registry to populate
its fresh cache. Native rebuilds, Windows acceptance, host application/device
validation and CI are separate operations.

The Rust candidate and backup rotation integration suites exercise catalog ordering,
raw-family limits, selection damage, non-destructive recovery and the invalidation
of held rotation tokens when checkpoints change. The Swift and .NET suites use a
native cursor and take held binary bytes through explicit restore. They also cover
their respective token transport, decoding, cancellation and result ownership contracts.
Rust C ABI tests exercise signed backup timestamps through reuse, stale-token rejection
and restoration without modifying the checkpoint. Swift and .NET test the native
timestamp and opaque payload projections.
Portable recovery-copy tests in Rust check relocation, regular-file replacement
and byte-for-byte preservation across rejections at the binding default limit.
Swift and .NET each exercise the 16 MiB default through their public APIs and check
that native integrity failures surface as typed errors.

`make lint` (also `mise run lint`) checks owned source and managed whitespace.
`mise run format` formats owned source. Static checks select source files using
Git and exclude native artifacts and original notices.

`make test` uses complete concurrency checks and warnings as errors against
UUID-owned temporary stores. Distribution checks verify the native receipt
against the source before testing the package. Rebuilding the framework requires
the explicit `make native` command.

`CARGO` and `SWIFT` accept executable paths. `DOTNET_BIN` selects .NET for binding
fixtures. Use command-scoped `PATH`, `RUSTFLAGS` and `DEVELOPER_DIR` for
provisioned tools. Keep a matching Rust toolchain on `PATH` for Cargo subcommands
and the native builder. Reuse `target/` and `.build/` between edits;
`CARGO_TARGET_DIR` can select a task-owned Rust directory. Native builds use `target/native`.

## Independent consumers and CI

`make test-consumer` runs `fixtures/consumers/swift` against a fresh copy of the
installable SDK, checking opaque-byte round trips and stale-generation rejection
through the public product. The maintainer target `make test-installed-consumers` runs the independent
Rust Git and installed C fixtures from a clean committed macOS checkout. Both
check opaque bytes and generation conflicts in temporary stores. The Rust fixture
resolves a new lock at the exact SDK commit and needs registry access to populate
its fresh cache; the C fixture uses a copied framework and notices.
These local gates establish source/artifact consumption, separately from remote
Git authentication or publication.

If your checkout is shallow, run the driver with a complete local repository or
bundle containing its exact commit:

```sh
mise run sdk-tooling -- rust-c-consumers \
  --git-source /absolute/verified/configuration-storage.bundle \
  --evidence /absolute/owned-artifacts/storage-installed-consumers.json
```

Choose an unused evidence path outside the checkout for the report and its
companion lock and metadata files. The report ties the checked-out SDK commit to
the isolated Rust and C fixture results.

The [macOS workflow](../.github/workflows/check-macos.yml) runs strict Swift
tests and the standalone SwiftPM consumer on macOS 26 with Xcode 26.6 and on
macOS 27 with Xcode 27.2. It also runs source lint, Rust checks and .NET binding
tests on macOS 27. The
[Windows workflow](../.github/workflows/check-windows.yml) runs Rust and .NET
SDK tests on Windows x64.

## Maintainer distribution checks

[sdk-checks.toml](../sdk-checks.toml) describes the SDK's fixtures and
documentation checks. [sdk-distribution.toml](../sdk-distribution.toml) lists the
release checks for each platform. The [public SDK Tooling workflow](https://github.com/naviapps/sdk-tooling/actions/workflows/check-sdk-distribution.yml)
verifies the native receipts and standalone Swift, Rust and C consumers. It
packs the verified macOS and Windows native libraries in one NuGet package and
uses the same package bytes for each independent .NET consumer. Dispatch it
with the full commit SHA of the SDK on `main`:

```sh
gh workflow run check-sdk-distribution.yml -R naviapps/sdk-tooling \
  -f sdk=configuration-storage -f "revision=$(git rev-parse HEAD)"
```

Run the local native and packaging checks with the SDK Tooling revision pinned
in [mise.toml](../mise.toml).

```sh
mise exec -- make check-distribution
```

The shared lint and installed Swift, Rust and C consumer checks use the pinned
public package.

See the [.NET guide](../bindings/dotnet/README.md#windows-x64-package-acceptance)
for the local Windows command, prerequisites and package output. Use an owned
environment for Windows ARM64 emulation checks.

## Native rebuilds and provenance

After editing the Rust FFI declarations or their documentation, run
`mise exec -- make generate-header`. The header check compares the tracked
file with the output of the pinned cbindgen generator. Consumers use the
bundled header.

[native.toml](../native.toml) declares the framework inputs, symbol prefix and
minimum macOS version. The explicit `make native` target requires pinned Rust
1.99.1, both Darwin
standard-library targets and locked offline
dependencies. It uses `target/native` and builds `libconfiguration_storage.dylib`
into a universal arm64/x86_64 framework with minimum macOS 26.0. Its receipt is
`Artifacts/native-build.json`; verification never refreshes it.

An ABI change requires matching native frameworks, headers, receipts and Windows
x64 DLLs, plus repacked per-runtime NuGet packages. Run binding and installed
consumer checks before advancing pins. Native builds and Windows acceptance
remain explicit maintainer operations.

Retain the production and build notice union for arm64/x86_64 macOS and Windows
x64, including Windows package notices during a macOS rebuild. The prior Windows
ARM64 graph and Windows x64 graph resolve the same locked production and build
dependencies, so the retained Windows notices cover the x64 package. The
`rusqlite 0.41.1` and `libsqlite3-sys 0.39.1` notices include the bundled SQLite
disclaimer from `libsqlite3-sys/sqlite3/sqlite3.h` as well as the crate's MIT license.

`ThirdParty/Rust` retains Rust 1.99.1's `LICENSE-APACHE`, `LICENSE-MIT`, `COPYRIGHT`
and `COPYRIGHT-library.html` with verified source/hash provenance. Builds use those
local files. Framework resources preserve all notices in `Resources/ThirdPartyNotices.txt`;
Windows/NuGet packages use the original local notice files.
