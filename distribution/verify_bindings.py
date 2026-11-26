#!/usr/bin/env python3
"""Test C/.NET and pack a private local package against an unchanged parent-built framework."""

import argparse
import hashlib
import json
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from sdk_tooling.adapters import load
from sdk_tooling.c_headers import prepare_generated
from sdk_tooling.dotnet import subprocess_environment
from sdk_tooling.xcframework import xcframework

from distribution.pack_nuget import NUGET_CONFIG, pack

ROOT = Path(__file__).resolve().parents[1]
TEST_PROJECT = (
    ROOT / "bindings/dotnet/tests/ConfigurationStorage.Tests/ConfigurationStorage.Tests.csproj"
)
MODULE = "CConfigurationStorage"


def run(*args, env, cwd=None):
    subprocess.run([str(a) for a in args], cwd=cwd or ROOT, env=env, check=True)


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


def managed_tests(dotnet, env, library, *, runtime=None):
    load("dotnet").run_managed(
        dotnet,
        TEST_PROJECT,
        env=env,
        cwd=ROOT,
        nuget_config=NUGET_CONFIG,
        arguments=("--native",),
        runtime=runtime,
        properties={
            "ConfigurationStorageNativeLibrary": str(library),
            "UseSharedCompilation": "false",
        },
        verify_style=True,
    )


def c_test(executable, env, output):
    with tempfile.TemporaryDirectory(dir=output, prefix="c-owned-") as directory:
        run(executable, Path(directory) / "store", env=env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--c-only", action="store_true")
    mode.add_argument("--no-package", action="store_true")
    args = parser.parse_args()
    env = dict(subprocess_environment(), DOTNET_CLI_UI_LANGUAGE="en")
    if sys.platform != "darwin":
        raise SystemExit("Use the Windows distribution fixture on an owned x64 or ARM64 worker")
    library = framework_binary()
    if not library.is_file():
        raise SystemExit(
            "Parent-built Artifacts/CConfigurationStorage.xcframework is missing; this helper never builds native libraries"
        )
    prepare_generated(ROOT, env=env, verify=True)
    native = xcframework(ROOT)
    artifacts, artifact = load("artifacts"), native.artifact
    before = artifacts.hashes(artifacts.files(artifact), artifact)
    native.verify(env)
    output = ROOT / "build/bindings"
    output.mkdir(parents=True, exist_ok=True)
    receipt = {
        "nativeSha256": hashlib.sha256(library.read_bytes()).hexdigest(),
        "notRun": ["Windows runtime", "host application integration"],
        "coverage": ["C owned-tempstore ABI"],
    }
    try:
        load("c_compiler").compile_c(
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
            managed_tests(dotnet, env, library)
            rid = "osx-arm64" if platform.machine().lower() in ("arm64", "aarch64") else "osx-x64"
            receipt["runtime"] = rid
            receipt["coverage"] += [
                "managed ownership/cancellation/disposal",
                "native disposable stores",
            ]
            if not args.no_package:
                package = pack(("osx-arm64", "osx-x64"), environment=env, native=native)
                run(*load("consumers.nuget").command(package, root=ROOT), env=env)
                receipt["packages"] = {package.name: artifacts.file_sha256(package)}
                receipt["coverage"].append("clean local package consumer")
    finally:
        if artifacts.hashes(artifacts.files(artifact), artifact) != before:
            raise SystemExit("Parent-built framework changed during bindings acceptance")
    (output / "acceptance.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print("Bindings acceptance passed against the unchanged parent-built framework.")


if __name__ == "__main__":
    main()
