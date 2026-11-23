"""ConfigurationStorage ABI fixture and package acceptance boundaries."""

import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
spec = importlib.util.spec_from_file_location(
    "configuration_storage_bindings", ROOT / "tools/bindings.py"
)
bindings = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bindings)
with patch.dict(sys.modules, {"bindings": bindings}):
    spec = importlib.util.spec_from_file_location(
        "configuration_storage_windows", ROOT / "tools/windows.py"
    )
    windows = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(windows)
artifacts = bindings.load("artifacts")


class SDKToolPolicyTests(unittest.TestCase):
    def test_c_test_owns_and_removes_only_disposable_child(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            observed = []

            def run(*args, **kwargs):
                self.assertEqual(args[0], "owned-test")
                child = Path(args[1])
                self.assertEqual(child.parent.parent, output)
                self.assertFalse(child.exists())
                child.mkdir()
                (child / "fixture").write_text("owned")
                observed.append(child)

            with patch.object(bindings, "run", side_effect=run):
                bindings.c_test("owned-test", {}, output)
            self.assertFalse(observed[0].parent.exists())

    def test_managed_fixture_uses_selected_feed_and_explicit_native_path(self):
        library = Path("/fixture/CConfigurationStorage")
        tooling = Mock()
        bindings.managed_tests("dotnet", {}, library, tooling=tooling, runtime="win-x64")
        run = tooling.run_managed
        self.assertEqual(run.call_args.args, ("dotnet", bindings.TEST_PROJECT))
        self.assertEqual(run.call_args.kwargs["nuget_config"], bindings.NUGET_CONFIG)
        self.assertEqual(run.call_args.kwargs["arguments"], ("--native-library", library))
        self.assertEqual(run.call_args.kwargs["runtime"], "win-x64")
        self.assertEqual(run.call_args.kwargs["properties"], {"UseAppHost": "false"})

    def test_missing_framework_fails_before_any_native_build(self):
        with tempfile.TemporaryDirectory() as directory:
            with (
                patch.object(bindings, "ROOT", Path(directory)),
                patch.object(sys, "platform", "darwin"),
                patch.object(sys, "argv", ["bindings.py"]),
                patch.object(bindings, "run") as run,
            ):
                with self.assertRaisesRegex(SystemExit, "Parent-built"):
                    bindings.main()
                run.assert_not_called()

    def test_windows_sqlite_build_preserves_prepared_flags_and_uses_disposable_target(self):
        env = {
            "CARGO_ENCODED_RUSTFLAGS": "-Ctarget-feature=+crt-static",
            "CARGO_HOME": "/fixture/cargo",
            "CARGO_INCREMENTAL": "1",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER": "/fixture/tools/link.exe",
            "CFLAGS_x86_64-pc-windows-msvc": "/MD",
            "LIBSQLITE3_SYS_USE_PKG_CONFIG": "1",
        }
        before = dict(env)
        result = windows.build_environment(env, Path("/fixture/target"))
        self.assertEqual(env, before)
        self.assertEqual(result["CARGO_TARGET_DIR"], str(Path("/fixture/target")))
        self.assertEqual(result["CARGO_INCREMENTAL"], "0")
        self.assertEqual(result["CFLAGS_x86_64_pc_windows_msvc"], "/MT")
        self.assertEqual(result["LIBSQLITE3_SYS_USE_PKG_CONFIG"], "0")
        self.assertNotIn("CFLAGS_x86_64-pc-windows-msvc", result)
        self.assertEqual(
            result["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"],
            env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"],
        )
        flags = result["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
        self.assertIn("-Ctarget-feature=+crt-static", flags)
        self.assertIn("--remap-path-prefix=/fixture/cargo=/cargo", flags)

    def test_windows_requires_verified_retained_notices_without_rewriting_them(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            retained = root / "ThirdParty/fixture-1.0.0/LICENSE"
            retained.parent.mkdir(parents=True)
            retained.write_bytes(b"owned notice")
            graph = Mock()
            graph.registry_notices.return_value = {
                "fixture-1.0.0": {"files": {"LICENSE": b"owned notice"}}
            }
            cargo = SimpleNamespace(
                CargoDependencies=Mock(return_value=graph), runtime_notices=Mock()
            )
            with patch.object(windows, "ROOT", root):
                before = artifacts.hashes(artifacts.files(root), root)
                windows.verify_notices(cargo, {})
                self.assertEqual(artifacts.hashes(artifacts.files(root), root), before)
                self.assertEqual(
                    cargo.CargoDependencies.call_args.args[2], ["x86_64-pc-windows-msvc"]
                )
                retained.write_bytes(b"changed retained notice")
                with self.assertRaisesRegex(SystemExit, "Parent must retain"):
                    windows.verify_notices(cargo, {})
                self.assertEqual(retained.read_bytes(), b"changed retained notice")

    def test_windows_cli_and_packer_reject_arm64_before_selecting_tools(self):
        with (
            patch.object(windows, "load") as load,
            patch("sys.stderr"),
            self.assertRaises(SystemExit),
        ):
            windows.main(["--architecture", "arm64"])
        load.assert_not_called()
        tooling = Mock()
        with self.assertRaisesRegex(ValueError, "win-x64"):
            bindings.pack_and_consume(
                "dotnet", Path("absent.dll"), "win-arm64", {}, Path("unused"), tooling=tooling
            )
        tooling.stage_native.assert_not_called()

    def test_windows_acceptance_inputs_cover_local_tools_and_package_license(self):
        support = bindings.support_inputs()
        self.assertTrue(support)
        inputs = bindings.source_inputs(artifacts)
        self.assertEqual({name: inputs[name] for name in support}, support)
        self.assertEqual(
            inputs["LICENSE"], hashlib.sha256((ROOT / "LICENSE").read_bytes()).hexdigest()
        )

    def test_windows_package_uses_x64_asset_and_isolated_x64_consumer(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            library = output / "configuration_storage.dll"
            library.write_bytes(b"selected native bytes")
            version = windows.tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"][
                "package"
            ]["version"]
            package = output / "packages" / f"{bindings.PACKAGE}.{version}.nupkg"

            def pack(*args, **kwargs):
                package.write_bytes(b"accepted package")

            tooling = Mock()
            tooling.stage_native.return_value = output / "native/win-x64/configuration_storage.dll"
            tooling.pack_project.side_effect = pack
            observed = bindings.pack_and_consume(
                "dotnet", library, "win-x64", {}, output, tooling=tooling
            )
            self.assertEqual(
                observed, {package.name: hashlib.sha256(package.read_bytes()).hexdigest()}
            )
            self.assertEqual(
                tooling.pack_project.call_args.kwargs["properties"][
                    "ConfigurationStorageRuntimeIdentifier"
                ],
                "win-x64",
            )
            self.assertEqual(
                tooling.audit_package.call_args.kwargs["runtime_files"],
                {"runtimes/win-x64/native/configuration_storage.dll": library},
            )
            self.assertEqual(tooling.run_isolated_consumer.call_args.kwargs["runtime"], "win-x64")

    def test_x64_output_rejects_a_changed_package_and_preserves_prior_pair(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            source = output / "candidate"
            source.mkdir()
            package = source / "NaviApps.ConfigurationStorage.0.1.0.nupkg"
            package.write_bytes(b"accepted package")
            receipt = {"packages": {package.name: hashlib.sha256(package.read_bytes()).hexdigest()}}
            destination = output / "win-x64"
            windows.seal_package(source, destination, receipt, artifacts)
            self.assertEqual(json.loads((destination / windows.RECEIPT).read_bytes()), receipt)
            package.write_bytes(b"unreviewed bytes")
            with self.assertRaisesRegex(ValueError, "receipt differ"):
                windows.seal_package(source, destination, receipt, artifacts)
            self.assertEqual((destination / package.name).read_bytes(), b"accepted package")
            sentinel = destination / "unowned.txt"
            sentinel.write_bytes(b"unrelated data")
            with self.assertRaisesRegex(ValueError, "unrelated files"):
                windows.seal_package(source, destination, receipt, artifacts)
            self.assertEqual(sentinel.read_bytes(), b"unrelated data")


if __name__ == "__main__":
    unittest.main()
