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

from sdk_tooling.adapters import load, support_inputs

ROOT = Path(__file__).resolve().parents[2]
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
            "fixtures/distribution/bindings.py",
            "fixtures/distribution/windows.py",
            "fixtures/distribution/nuget/Program.cs",
            "fixtures/c-consumer/main.c",
        )
    ]
    return artifacts.hashes(paths, ROOT) | support_inputs(ROOT)


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
    # macOS keeps the portable fixture. On Windows require the explicit x64 package asset.
    tooling.run_isolated_consumer(
        dotnet,
        packages=[package],
        references={PACKAGE: version},
        source=(Path(__file__).parent / "nuget/Program.cs").read_text(encoding="utf-8"),
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
        raise SystemExit("Use the Windows distribution fixture on an owned x64 or ARM64 worker")
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
