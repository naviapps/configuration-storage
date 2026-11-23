#!/usr/bin/env python3
"""Test C/.NET and pack a private local package against an unchanged parent-built framework."""

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

from _tooling import load, support_inputs

ROOT = Path(__file__).resolve().parent.parent
PACKAGE = "NaviApps.ConfigurationStorage"
PROJECT = ROOT / "bindings/dotnet/src" / PACKAGE / (PACKAGE + ".csproj")
TEST_PROJECT = (
    ROOT / "bindings/dotnet/tests/ConfigurationStorage.Tests/ConfigurationStorage.Tests.csproj"
)
NUGET_CONFIG = ROOT / "bindings/dotnet/tests/ConfigurationStorage.Tests/NuGet.Config"
MODULE = "CConfigurationStorage"
NATIVE_FILENAMES = {
    "osx-arm64": "libconfiguration_storage.dylib",
    "osx-x64": "libconfiguration_storage.dylib",
    "win-x64": "configuration_storage.dll",
}


def run(*args, env, cwd=None):
    subprocess.run([str(a) for a in args], cwd=cwd or ROOT, env=env, check=True)


def environment():
    return dict(
        os.environ,
        DOTNET_CLI_TELEMETRY_OPTOUT="1",
        DOTNET_NOLOGO="1",
        DOTNET_CLI_UI_LANGUAGE="en",
        DOTNET_CLI_USE_MSBUILD_SERVER="0",
        MSBUILDDISABLENODEREUSE="1",
    )


def dotnet_tool(env):
    dotnet = env.get("DOTNET_BIN") or shutil.which("dotnet", path=env.get("PATH"))
    if not dotnet:
        raise SystemExit("Set DOTNET_BIN to the provisioned task-scoped .NET 10 executable")
    version = subprocess.check_output([dotnet, "--version"], env=env, text=True).strip()
    if not version.startswith("10."):
        raise SystemExit("Use .NET 10")
    return dotnet


def framework_binary():
    return (
        ROOT
        / "Artifacts"
        / (MODULE + ".xcframework")
        / "macos-arm64_x86_64"
        / (MODULE + ".framework")
        / MODULE
    )


def source_inputs(artifacts):
    paths = [
        ROOT / name
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "mise.toml", "LICENSE")
    ]
    paths += [p for p in (ROOT / "crates").rglob("*") if p.is_file() and p.name != ".DS_Store"]
    paths += [p for p in (ROOT / "ThirdParty").rglob("*") if p.is_file() and p.name != ".DS_Store"]
    paths += [
        p
        for p in (ROOT / "bindings/dotnet").rglob("*")
        if p.is_file()
        and p.name != ".DS_Store"
        and not {"bin", "obj"}.intersection(p.relative_to(ROOT / "bindings/dotnet").parts)
    ]
    paths += [
        ROOT / name
        for name in (
            "tools/bindings.py",
            "tools/windows.py",
            "fixtures/c-consumer/main.c",
        )
    ]
    return artifacts.hashes(paths, ROOT) | support_inputs()


def managed_tests(dotnet, env, library=None, *, tooling, runtime=None):
    arguments = ("--native-library", library) if library else ()
    tooling.run_managed(
        dotnet,
        TEST_PROJECT,
        env=env,
        cwd=ROOT,
        nuget_config=NUGET_CONFIG,
        arguments=arguments,
        runtime=runtime,
        properties={"UseAppHost": "false"} if runtime else None,
        verify_style=True,
    )


def c_test(executable, env, output):
    with tempfile.TemporaryDirectory(dir=output, prefix="c-owned-") as directory:
        run(executable, Path(directory) / "store", env=env)


def pack_and_consume(dotnet, library, rid, env, output, *, tooling):
    if rid not in NATIVE_FILENAMES:
        raise ValueError("ConfigurationStorage packages support osx-arm64, osx-x64 and win-x64")
    filename = NATIVE_FILENAMES[rid]
    staged = tooling.stage_native(library, output / "native" / rid / filename)
    # Notices are supplied and maintained by the native owner, never generated here.
    notices = {
        "licenses/" + path.relative_to(ROOT / "ThirdParty").as_posix(): path
        for path in (ROOT / "ThirdParty").rglob("*")
        if path.is_file() and path.name not in (".DS_Store", "registry-notices.json")
    }
    if not notices:
        raise SystemExit("Parent-maintained ThirdParty notices are required before packaging")
    packages = output / "packages"
    packages.mkdir(parents=True, exist_ok=True)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    tooling.pack_project(
        dotnet,
        PROJECT,
        output=packages,
        env=env,
        cwd=ROOT,
        nuget_config=NUGET_CONFIG,
        properties={
            "Version": version,
            "ConfigurationStorageRuntimeIdentifier": rid,
            "ConfigurationStorageNativeLibrary": str(staged),
        },
    )
    package = packages / (PACKAGE + "." + version + ".nupkg")
    tooling.audit_package(
        package,
        expected_files={"LICENSE": ROOT / "LICENSE", **notices},
        runtime_files={"runtimes/" + rid + "/native/" + filename: library},
    )
    consumer_source = """using NaviApps.ConfigurationStorage;
var directory = Path.Combine(Path.GetTempPath(), "configuration-storage-consumer-" + Guid.NewGuid());
try {
 var storeDirectory = Path.Combine(directory, "store");
 using var store = ConfigurationStore.CreateLazy(new Configuration(storeDirectory, "package-test"));
 using var existing = ConfigurationStore.CreateLazyExisting(new Configuration(storeDirectory, "package-test"));
 if (Directory.Exists(storeDirectory)) throw new Exception("Constructing a facade opened a store.");
 try { await existing.LoadAsync(); throw new Exception("Existing-only admission succeeded for a missing store."); }
 catch (StorageException error) when (error.Failure.Code == ErrorCode.StoreMissing) { }
 if (Directory.Exists(storeDirectory)) throw new Exception("Existing-only admission created a store.");
 if ((await store.LoadAsync()).IsInitialized) throw new Exception("Expected an empty disposable store.");
 var input = new byte[] {0, 255, 42};
 var writing = store.SaveAsync(null, input);
 input[0] = 99;
 var saved = await writing;
 if (await existing.LoadAsync() != saved) throw new Exception("The existing store did not reopen after creation.");
 if (saved.Payload![0] != 0) throw new Exception("Input bytes were borrowed.");
 saved.Payload![0] = 99;
 if (saved.Payload![0] != 0) throw new Exception("Snapshot bytes were borrowed.");
 var preparation = await store.PrepareBackupAsync(saved.Generation!);
 if (preparation.Backup is not null) throw new Exception("Expected no prior checkpoint in this disposable store.");
 var backup = await store.CreateBackupAsync(saved.Generation!, preparation.Token);
 await store.SaveAsync(saved.Generation, ReadOnlyMemory<byte>.Empty);
 try { await store.RestoreBackupAsync(backup.Token); throw new Exception("Stale validation token overwrote a newer write."); }
 catch (StorageException error) when (error.Failure.Code == ErrorCode.BackupChanged) { }
 var marker = Path.Combine(storeDirectory, "active-backup");
 var backups = Path.Combine(storeDirectory, "Backups");
 var selected = Path.Combine(backups, File.ReadAllText(marker) + ".sqlite");
 File.Move(selected, Path.Combine(backups, "snapshot.sqlite"));
 File.Delete(marker);
 var metadata = Path.Combine(backups, "desktop.ini");
 var metadataBytes = new byte[] {4, 5, 6};
 File.WriteAllBytes(metadata, metadataBytes);
 var validated = await store.ReadBackupAsync();
 if (!validated.Snapshot.Payload!.SequenceEqual(saved.Payload!)) throw new Exception("Backup semantic fixture validation failed.");
 if (validated != await store.ReadBackupAsync()) throw new Exception("Equal native backup observations compared unequal.");
 var restored = await store.RestoreBackupAsync(validated.Token);
 if (!restored.Payload!.SequenceEqual(saved.Payload!)) throw new Exception("Opaque bytes changed.");
 var observations = await Task.WhenAll(Enumerable.Range(0, 16).Select(_ => store.LoadAsync()));
 if (observations.Any(value => value != restored) || observations.Append(restored).ToHashSet().Count != 1)
     throw new Exception("Equal native snapshot observations compared unequal.");
 var page = await store.ListSnapshotCandidatesAsync();
 if (page != await store.ListSnapshotCandidatesAsync()) throw new Exception("Equal candidate pages compared unequal.");
 var candidateId = page.Candidates.First(reference => reference.IsSelected == true).Id;
 var candidate = await store.ReadSnapshotCandidateAsync(candidateId);
 if (candidate != await store.ReadSnapshotCandidateAsync(candidateId)) throw new Exception("Equal candidate observations compared unequal.");
 using var cancelled = new CancellationTokenSource();
 cancelled.Cancel();
 try { await store.SaveAsync(restored.Generation, new byte[] {1}, cancelled.Token); throw new Exception("Pre-cancelled write succeeded."); }
 catch (OperationCanceledException) { }
 if ((await store.LoadAsync()).Generation != restored.Generation) throw new Exception("Pre-cancelled write changed storage.");
 var oldBackupId = page.Candidates.First(reference => reference.Source == SnapshotCandidateSource.Backup && reference.IsSelected == true).Id;
 var current = await store.SaveAsync(restored.Generation, "{}"u8.ToArray());
 var rebuild = await store.PrepareBackupRebuildAsync(current.Generation!);
 var rebuilt = await store.RebuildBackupAsync(current.Generation!, rebuild);
 var previous = await store.ReadSnapshotCandidateAsync(oldBackupId);
 if (rebuilt.Snapshot != current || previous.Snapshot != validated.Snapshot || previous.IsSelected != false)
     throw new Exception("Explicit reconstruction did not retain the earlier opaque checkpoint.");
 if (!File.ReadAllBytes(metadata).SequenceEqual(metadataBytes))
     throw new Exception("Recovery changed desktop metadata.");
 var destination = Path.Combine(directory, "portable.copy");
 var opaque = new byte[] {0, 255, 128};
 var oversized = new byte[] {0, 255, 128, 1};
 await ConfigurationRecoveryCopy.WriteAsync(destination, opaque, opaque.Length);
 try { await ConfigurationRecoveryCopy.WriteAsync(destination, oversized, opaque.Length); throw new Exception("Oversized portable write was accepted."); }
 catch (StorageException error) when (error.Failure.Code == ErrorCode.InvalidConfiguration) { }
 try { await ConfigurationRecoveryCopy.WriteAsync(destination, oversized, opaque.Length, cancelled.Token); throw new Exception("Pre-cancelled portable write was accepted."); }
 catch (OperationCanceledException) { }
 var signedDate = DateTime.UnixEpoch.AddTicks(-12_500_000);
 File.SetLastWriteTimeUtc(destination, signedDate);
 var reviewed = await ConfigurationRecoveryCopy.ReadAsync(destination, opaque.Length);
 if (!reviewed.Payload.SequenceEqual(opaque)) throw new Exception("Rejected portable write changed the retained copy.");
 if (reviewed.ModifiedAt != -1.25) throw new Exception("The copy lost its signed fractional date.");
 if (reviewed != await ConfigurationRecoveryCopy.ReadAsync(destination, opaque.Length)) throw new Exception("Equal portable copies compared unequal.");
 File.SetLastWriteTimeUtc(destination, DateTime.UnixEpoch.AddTicks(1));
 var early = await ConfigurationRecoveryCopy.ReadAsync(destination, opaque.Length);
 File.SetLastWriteTimeUtc(destination, DateTime.UnixEpoch.AddTicks(2));
 var later = await ConfigurationRecoveryCopy.ReadAsync(destination, opaque.Length);
 if (early.ModifiedAt != 1e-7 || later.ModifiedAt != 2e-7 || early == later
     || early.Digest != reviewed.Digest || later.Digest != reviewed.Digest)
     throw new Exception("Portable copy observations lost their distinct fractional epoch dates.");
 var portableConfiguration = new Configuration(Path.Combine(directory, "portable-restore"), "com.example.portable-restore");
 using var portableStore = await ConfigurationStore.OpenAsync(portableConfiguration);
 var original = await portableStore.SaveAsync(null, new byte[] {77});
 var originalPage = await portableStore.ListSnapshotCandidatesAsync(source: SnapshotCandidateSource.Generation);
 var originalId = originalPage.Candidates.Single().Id;
 var token = await portableStore.PrepareRestoreAsync(reviewed.Payload);
 var replacement = new byte[] {1, 254, 127};
 await ConfigurationRecoveryCopy.WriteAsync(destination, replacement);
 var obstruction = Path.Combine(portableConfiguration.Directory, "Recovery");
 const string sentinel = "unavailable recovery directory";
 File.WriteAllText(obstruction, sentinel);
 try {
     await portableStore.RestoreSnapshotAsync(reviewed.Payload, token);
     throw new Exception("Restoration published before preserving the original selector.");
 } catch (StorageException error) when (error.Failure.Code == ErrorCode.InvalidStoredData) { }
 if (await portableStore.LoadAsync() != original || File.ReadAllText(obstruction) != sentinel)
     throw new Exception("Failed restore changed the original data.");
 File.Delete(obstruction);
 var portableRestored = await portableStore.RestoreSnapshotAsync(reviewed.Payload, token);
 using var reopened = await ConfigurationStore.OpenExistingAsync(portableConfiguration);
 var loaded = await reopened.LoadAsync();
 if (!portableRestored.IsInitialized || loaded.Generation != portableRestored.Generation || !loaded.Payload!.SequenceEqual(opaque))
     throw new Exception("Restoration did not persist the reviewed payload.");
 var originalCandidate = await reopened.ReadSnapshotCandidateAsync(originalId);
 if (originalCandidate.Snapshot != original || originalCandidate.IsSelected != false)
     throw new Exception("Restoration did not retain the original generation.");
 var retained = await ConfigurationRecoveryCopy.ReadAsync(destination);
 if (!retained.Payload.SequenceEqual(replacement) || retained.Digest == reviewed.Digest)
     throw new Exception("Restoration changed the replacement portable copy.");

 var queue = new SnapshotSaveQueue<int>(_ => Task.CompletedTask);
 var idle = queue.State;
 if (idle != queue.State) throw new Exception("Equal idle save queue observations compared unequal.");
 var revision = queue.Enqueue(1);
 var pending = queue.State;
 if (pending != queue.State || pending == idle || pending.PendingRevision != revision || !pending.HasUnsavedChanges)
     throw new Exception("Save queue observations did not reflect pending work.");
 await queue.FlushAsync();
 var committed = queue.State;
 if (committed != queue.State || committed == pending || committed.CommittedRevision != revision || committed.HasUnsavedChanges)
     throw new Exception("Save queue observations did not reflect the committed revision.");
 if (new HashSet<SnapshotSaveState> { idle, pending, committed, queue.State }.Count != 3)
     throw new Exception("Equal save queue observations did not hash consistently.");
} finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
Console.WriteLine("Clean local NuGet consumer passed token validation, copied bytes, serialized reads and reviewed portable restoration.");
"""
    # macOS keeps the portable fixture. On Windows require the explicit x64 package asset.
    tooling.run_isolated_consumer(
        dotnet,
        packages=[package],
        references={PACKAGE: version},
        source=consumer_source,
        work_parent=output,
        env=env,
        exclude_paths=(ROOT / "target", output),
        runtime=rid if rid == "win-x64" else None,
    )
    return {package.name: hashlib.sha256(package.read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--c-only", action="store_true")
    mode.add_argument("--dotnet-only", action="store_true")
    mode.add_argument(
        "--managed-only",
        action="store_true",
        help="Run fake-native tests without requiring native artifacts",
    )
    args = parser.parse_args()
    env = environment()
    if args.managed_only:
        managed_tests(dotnet_tool(env), env, tooling=load("bindings"))
        return
    if sys.platform != "darwin":
        raise SystemExit("Use tools/windows.py to build x64 on an owned Windows worker")
    library = framework_binary()
    if not library.is_file():
        raise SystemExit(
            "Parent-built Artifacts/CConfigurationStorage.xcframework is missing; this helper never builds native libraries"
        )
    tooling = load("bindings")
    artifacts = load("artifacts")
    artifact = ROOT / "Artifacts" / (MODULE + ".xcframework")
    before = artifacts.hashes(artifacts.files(artifact), artifact)
    output = ROOT / "build/bindings"
    output.mkdir(parents=True, exist_ok=True)
    receipt = {
        "nativeSha256": hashlib.sha256(library.read_bytes()).hexdigest(),
        "notRun": ["Windows runtime", "host application integration"],
    }
    try:
        if not args.dotnet_only:
            tooling.compile_c(
                ROOT / "crates/ffi/tests/c/abi.c",
                compiler=("xcrun", "clang"),
                output=output / "abi",
                include_dirs=(ROOT / "crates/ffi/include",),
                libraries=(library,),
                standard="c17",
                flags=("-Wl,-rpath," + str(library.parent.parent),),
                env=env,
                cwd=ROOT,
            )
            c_test(output / "abi", env, output)
        if not args.c_only:
            dotnet = dotnet_tool(env)
            managed_tests(dotnet, env, library, tooling=tooling)
            rid = "osx-arm64" if platform.machine().lower() in ("arm64", "aarch64") else "osx-x64"
            receipt["packages"] = pack_and_consume(
                dotnet, library, rid, env, output, tooling=tooling
            )
            receipt["runtime"] = rid
        receipt["coverage"] = (["C owned-tempstore ABI"] if not args.dotnet_only else []) + (
            [
                "managed ownership/cancellation/disposal",
                "native disposable stores",
                "clean local package consumer",
            ]
            if not args.c_only
            else []
        )
    finally:
        if artifacts.hashes(artifacts.files(artifact), artifact) != before:
            raise SystemExit("Parent-built framework changed during bindings acceptance")
    (output / "acceptance.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print("Bindings acceptance passed against the unchanged parent-built framework.")


if __name__ == "__main__":
    main()
