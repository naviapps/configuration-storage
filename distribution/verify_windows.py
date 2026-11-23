#!/usr/bin/env python3
"""Verify the x64 ConfigurationStorage library and its independent consumers."""

import argparse
import shutil
import tempfile
from pathlib import Path

from sdk_tooling.adapters import load, support_inputs
from sdk_tooling.native_plan import ffi_product
from sdk_tooling.windows_delivery import (
    RID,
    X64Acceptance,
    acceptance_source,
    publish_package_evidence,
)

from distribution import verify_bindings as bindings
from distribution.pack_nuget import pack

ROOT = Path(__file__).resolve().parents[1]
RECEIPT = "configuration-storage-windows.json"


def configure_sqlite(environment):
    target_flags = {"cflags_x86_64-pc-windows-msvc", "cflags_x86_64_pc_windows_msvc"}
    env = {key: value for key, value in environment.items() if key.lower() not in target_flags}
    # Build the bundled SQLite C code against the same static CRT as Rust.
    env.update(
        CFLAGS_x86_64_pc_windows_msvc="/MT",
        LIBSQLITE3_SYS_USE_PKG_CONFIG="0",
    )
    return env


def run_c_fixtures(candidate, acceptance, import_library):
    compiler = load("c_compiler")
    windows = load("windows")
    artifacts = load("artifacts")
    root = acceptance.root
    header = root / "crates/ffi/include/configuration_storage.h"
    installed = artifacts.stage_native(header, candidate / header.name)

    for name, source, include in (
        ("abi", root / "crates/ffi/tests/c/abi.c", header.parent),
        ("c-consumer", root / "fixtures/consumers/c/main.c", installed.parent),
    ):
        executable = candidate / (name + ".exe")
        compiler.compile_c(
            source,
            output=executable,
            compiler=[acceptance.tools / "cl.exe"],
            env=acceptance.env,
            cwd=root,
            style="msvc",
            standard="c17",
            flags=("/MT", "/D_CRT_SECURE_NO_WARNINGS"),
            include_dirs=[include],
            libraries=[import_library],
        )
        windows.require_pe_machine(executable, "x64")
        if name == "abi":
            bindings.c_test(executable, acceptance.env, candidate)
        else:
            with tempfile.TemporaryDirectory(dir=candidate, prefix="c-installed-") as fixture:
                bindings.run(executable, fixture, env=acceptance.env)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--native-only",
        action="store_true",
        help="Export the checked native library for a subsequent combined package",
    )
    args = parser.parse_args(argv)
    artifact_root = load("vm_environment").artifacts_dir()
    artifacts, windows = (load(name) for name in ("artifacts", "windows"))
    acceptance = X64Acceptance(ROOT)
    acceptance.env = configure_sqlite(acceptance.env)
    before = acceptance_source(ROOT, match_actions=True), support_inputs(ROOT)
    native = ffi_product(ROOT)
    built = acceptance.build(native)
    with tempfile.TemporaryDirectory(dir=built.parent, prefix="c-consumer-") as directory:
        candidate = Path(directory)
        library, import_library, native_receipt = acceptance.stage(native, built, candidate)
        run_c_fixtures(candidate, acceptance, import_library)
        bindings.managed_tests(acceptance.dotnet, acceptance.env, library, runtime=RID)
        expected = {
            path.name: native_receipt["outputs"][path.name] for path in (library, import_library)
        }
        if artifacts.hashes((library, import_library), candidate) != expected:
            raise ValueError("The verified native library changed during consumer acceptance")
    package = None
    if not args.native_only:
        package = pack((RID,), environment=acceptance.env)
        bindings.run(
            *load("consumers.nuget").command(package, "--windows-architecture", "x64"),
            env=acceptance.env,
        )
    if (acceptance_source(ROOT, match_actions=True), support_inputs(ROOT)) != before:
        raise ValueError("SDK inputs changed during Windows acceptance")
    if windows.verify_artifact(native, "x64", environment=acceptance.env)[1] != native_receipt:
        raise ValueError("The native build changed during Windows acceptance")

    if artifact_root is not None:
        destination = artifact_root / "sdk-windows" / RID
        if destination.exists() or destination.is_symlink():
            raise FileExistsError("Windows evidence destination already exists")
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=destination.parent, prefix="candidate-") as directory:
            staged = Path(directory) / RID
            shutil.copytree(built, staged)
            receipt_path = staged / "native-build.json"
            expected = native_receipt["outputs"] | {
                receipt_path.name: artifacts.file_sha256(built / receipt_path.name)
            }
            if artifacts.hashes(artifacts.files(staged), staged) != expected:
                raise ValueError("Exported Windows files differ from the accepted native build")
            staged.rename(destination)
    if package is not None:
        packages = {package.name: artifacts.file_sha256(package)}
        receipt = {
            "source": before[0][0],
            "target": native_receipt["target"],
            "runtime": RID,
            "hostArchitecture": acceptance.host,
            "executionMode": "x64-emulation" if acceptance.host == "arm64" else "native",
            "dotnetVersion": acceptance.dotnet_version,
            "abiVersions": native_receipt["abiVersions"],
            "nativeSha256": native_receipt["outputs"][library.name],
            "packages": packages,
            "windowsRuntimeTested": True,
        }
        with tempfile.TemporaryDirectory(dir=built.parent, prefix="local-package-") as directory:
            candidate = Path(directory)
            artifacts.stage_file(package, candidate / package.name)
            publish_package_evidence(
                candidate,
                receipt,
                receipt_name=RECEIPT,
                destination=(artifact_root or (ROOT / "build/windows")) / RID,
            )
    print(f"Windows {RID} static-CRT acceptance passed on {acceptance.host}.")


if __name__ == "__main__":
    main()
