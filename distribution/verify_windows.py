#!/usr/bin/env python3
"""Build and verify the x64 ConfigurationStorage package on an owned Windows worker."""

import argparse
import hashlib
import os
import re
import subprocess
import tempfile
import tomllib
from pathlib import Path

from sdk_tooling.adapters import git, load
from sdk_tooling.cargo_build import compile_units, verify_units
from sdk_tooling.native_plan import ffi_product
from sdk_tooling.windows_delivery import (
    RID,
    TARGET,
    X64Acceptance,
    locked_fetch_args,
    publish_package_evidence,
)

from distribution import verify_bindings as bindings

ROOT = Path(__file__).resolve().parents[1]
RECEIPT = "configuration-storage-windows.json"


def verify_notices(cargo, env):
    """Verify parent-maintained notices; never regenerate or prune ThirdParty."""
    notices = cargo.CargoDependencies(
        ROOT, ["configuration-storage-ffi"], [TARGET], env
    ).registry_notices()
    cargo.runtime_notices(ROOT, env)
    for directory, record in notices.items():
        for relative, data in record["files"].items():
            retained = ROOT / "ThirdParty" / directory / relative
            if not retained.is_file() or retained.read_bytes() != data:
                raise SystemExit(
                    "Parent must retain Windows graph notices before packaging: "
                    + directory
                    + "/"
                    + relative
                )


def build_environment(env, target_directory):
    # The candidate target is removed after verification, so no later build reuses incremental state.
    result = dict(env, CARGO_TARGET_DIR=str(target_directory), CARGO_INCREMENTAL="0")
    flags = [
        *result["CARGO_ENCODED_RUSTFLAGS"].split("\x1f"),
        "--remap-path-prefix=" + str(ROOT) + "=/configuration-storage",
        "--remap-path-prefix=" + str(Path.home()) + "=/developer",
    ]
    if result.get("CARGO_HOME"):
        flags.append("--remap-path-prefix=" + result["CARGO_HOME"] + "=/cargo")
    result["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    # cc-rs builds bundled SQLite; Rust's target-feature also selects its static CRT.
    for key in tuple(result):
        if key.lower() in ("cflags_x86_64-pc-windows-msvc", "cflags_x86_64_pc_windows_msvc"):
            del result[key]
    result["CFLAGS_x86_64_pc_windows_msvc"] = "/MT"
    result["LIBSQLITE3_SYS_USE_PKG_CONFIG"] = "0"
    return result


def checkout_commit(env):
    commit = git(ROOT, "rev-parse", "--verify", "HEAD^{commit}").decode().strip()
    if not re.fullmatch(r"[a-f0-9]{40}", commit):
        raise ValueError("Windows acceptance requires an exact Git commit")
    if env.get("GITHUB_ACTIONS") == "true" and (
        commit != env.get("GITHUB_SHA")
        or git(ROOT, "status", "--porcelain=v1", "--untracked-files=all").strip()
    ):
        raise ValueError("Windows acceptance requires the clean Actions source commit")
    return commit


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args(argv)
    windows, artifacts, c_compiler = (load(name) for name in ("windows", "artifacts", "c_compiler"))
    output = ROOT / "build/windows"
    artifact_root = load("vm_environment").artifacts_dir() or output
    dotnet_path = Path(os.environ.get("DOTNET_BIN", ""))
    if not dotnet_path.is_absolute() or not dotnet_path.is_file():
        raise ValueError("Set DOTNET_BIN to the absolute path of the pinned x64 .NET SDK")
    windows.require_pe_machine(dotnet_path, "x64")
    acceptance = X64Acceptance(ROOT)
    host, env, tools, dotnet = acceptance.host, acceptance.env, acceptance.tools, acceptance.dotnet
    pinned_rust = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    compiler_info = subprocess.check_output(
        ["rustc", "--version", "--verbose"], cwd=ROOT, env=env, text=True
    )
    compiler = compiler_info.splitlines()[0]
    if (
        not compiler.startswith("rustc " + pinned_rust + " ")
        or "host: " + windows.MSVC_TARGETS[host] not in compiler_info.splitlines()
    ):
        raise ValueError("Use the pinned Rust compiler built for the Windows host architecture")
    windows.require_rust_target_stdlib(
        subprocess.check_output(
            ["rustc", "--print", "target-libdir", "--target", TARGET], cwd=ROOT, env=env, text=True
        ).strip(),
        TARGET,
    )
    output.mkdir(parents=True, exist_ok=True)
    with (
        artifacts.build_lock(output),
        tempfile.TemporaryDirectory(dir=output, prefix="win-x64-build-") as directory,
    ):
        candidate = Path(directory)
        before = (checkout_commit(env), bindings.source_inputs(artifacts))
        # The committed macOS artifact and parent-maintained notices are read-only here.
        bindings.run(*locked_fetch_args(notice_targets=()), env=env)
        cargo = load("cargo")
        verify_notices(cargo, env)
        build_env = build_environment(env, candidate / "target")
        bindings.run("cargo", "fmt", "--all", "--", "--check", env=build_env)
        bindings.run(
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--target",
            TARGET,
            "--locked",
            "--offline",
            "--",
            "-D",
            "warnings",
            env=build_env,
        )
        bindings.run(
            "cargo",
            "test",
            "--workspace",
            "--target",
            TARGET,
            "--locked",
            "--offline",
            env=build_env,
        )
        release = candidate / "target" / TARGET / "release"
        product = ffi_product(ROOT)
        graph = cargo.CargoDependencies(ROOT, [product.package], [TARGET], build_env)
        dependencies = graph.provenance()
        units = compile_units(
            [
                "cargo",
                "build",
                "-p",
                product.package,
                "--release",
                "--target",
                TARGET,
                "--locked",
                "--offline",
                "--message-format=json-render-diagnostics",
            ],
            graph=graph,
            target=TARGET,
            library=release / "configuration_storage.dll",
            env=build_env,
        )
        cargo_build = {TARGET: units}
        verify_units(cargo_build, dependencies, [TARGET], product.package, product.library)
        header = ROOT / "crates/ffi/include/configuration_storage.h"
        library = artifacts.stage_native(
            release / "configuration_storage.dll", candidate / "configuration_storage.dll"
        )
        import_library = artifacts.stage_native(
            release / "configuration_storage.dll.lib", candidate / "configuration_storage.dll.lib"
        )
        native_hash = hashlib.sha256(library.read_bytes()).hexdigest()
        exports = windows.verify_library(
            library, [header], "configuration_storage_", tools, build_env, "x64"
        )
        windows.probe_abi(product, library, import_library, tools, build_env, "x64")

        def compile_fixture(source, name, include):
            executable = candidate / name
            c_compiler.compile_c(
                ROOT / source,
                output=executable,
                compiler=[tools / "cl.exe"],
                env=build_env,
                cwd=ROOT,
                style="msvc",
                standard="c17",
                flags=("/MT", "/D_CRT_SECURE_NO_WARNINGS"),
                include_dirs=[include],
                libraries=[import_library],
            )
            windows.require_pe_machine(executable, "x64")
            return executable

        executable = compile_fixture("crates/ffi/tests/c/abi.c", "abi.exe", header.parent)
        bindings.c_test(executable, build_env, candidate)
        installed_header = artifacts.stage_native(header, candidate / header.name)
        executable = compile_fixture(
            "fixtures/consumers/c/main.c", "c-consumer.exe", installed_header.parent
        )
        # The installed harness takes the parent of its fixture store.
        with tempfile.TemporaryDirectory(dir=candidate, prefix="c-installed-") as fixture:
            bindings.run(executable, fixture, env=build_env)
        bindings.managed_tests(dotnet, build_env, library, runtime=RID)
        packages = bindings.pack_and_consume(dotnet, library, RID, build_env, candidate)
        if (checkout_commit(env), bindings.source_inputs(artifacts)) != before:
            raise SystemExit("Native source inputs changed during Windows acceptance")
        if graph.provenance() != dependencies:
            raise ValueError("Native dependencies changed during Windows acceptance")
        if hashlib.sha256(library.read_bytes()).hexdigest() != native_hash:
            raise ValueError("The staged DLL changed during Windows x64 acceptance")
        receipt = {
            "source": before[0],
            "target": TARGET,
            "runtime": RID,
            "hostArchitecture": host,
            "executionMode": "x64-emulation" if host == "arm64" else "native",
            "dotnetVersion": acceptance.dotnet_version,
            "compiler": compiler,
            "inputs": before[1],
            "dependencies": dependencies,
            "cargoBuild": cargo_build,
            "exports": sorted(exports),
            "abiVersions": product.abi_versions(windows=True),
            "abiProbeArchitecture": "x64",
            "nativeSha256": native_hash,
            "staticCrt": True,
            "packages": packages,
            "coverage": [
                "Windows native Rust tests/Clippy",
                "C disposable store ABI/ownership and standalone consumer",
                "managed serialization/cancellation/disposal",
                "native disposable stores",
                "clean local NuGet consumer",
            ],
            "notRun": ["host application integration"],
        }
        publish_package_evidence(
            candidate / "packages",
            receipt,
            receipt_name=RECEIPT,
            destination=artifact_root / RID,
        )
    print(f"Windows {RID} static-CRT acceptance passed on {host} ({receipt['executionMode']}).")


if __name__ == "__main__":
    main()
