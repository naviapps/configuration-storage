"""ConfigurationStorage ABI fixture and package acceptance boundaries."""

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from distribution import pack_nuget
from distribution import verify_bindings as bindings
from distribution import verify_windows as windows


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
        dotnet = Mock()
        with patch.object(bindings, "load", return_value=dotnet) as load:
            bindings.managed_tests("dotnet", {}, library, runtime="win-x64")
        load.assert_called_once_with("dotnet")
        run = dotnet.run_managed
        self.assertEqual(run.call_args.args, ("dotnet", bindings.TEST_PROJECT))
        self.assertEqual(run.call_args.kwargs["nuget_config"], bindings.NUGET_CONFIG)
        self.assertEqual(run.call_args.kwargs["arguments"], ("--native",))
        self.assertEqual(run.call_args.kwargs["runtime"], "win-x64")
        self.assertEqual(
            run.call_args.kwargs["properties"], {"ConfigurationStorageNativeLibrary": str(library)}
        )

    def test_missing_framework_fails_before_any_native_build(self):
        with tempfile.TemporaryDirectory() as directory:
            with (
                patch.object(bindings, "ROOT", Path(directory)),
                patch.object(sys, "platform", "darwin"),
                patch.object(sys, "argv", ["verify_bindings.py"]),
                patch.object(bindings, "run") as run,
            ):
                with self.assertRaisesRegex(SystemExit, "Parent-built"):
                    bindings.main()
                run.assert_not_called()

    def test_windows_sqlite_uses_static_crt_without_overwriting_worker_environment(self):
        env = {
            "CARGO_ENCODED_RUSTFLAGS": "-Ctarget-feature=+crt-static",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER": "/fixture/tools/link.exe",
            "CFLAGS_x86_64-pc-windows-msvc": "/MD",
            "LIBSQLITE3_SYS_USE_PKG_CONFIG": "1",
        }
        before = dict(env)
        result = windows.configure_sqlite(env)
        self.assertEqual(env, before)
        self.assertEqual(result["CFLAGS_x86_64_pc_windows_msvc"], "/MT")
        self.assertEqual(result["LIBSQLITE3_SYS_USE_PKG_CONFIG"], "0")
        self.assertNotIn("CFLAGS_x86_64-pc-windows-msvc", result)
        self.assertEqual(
            result["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"],
            env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"],
        )

    def test_packer_rejects_arm64_windows_before_artifact_inspection(self):
        with (
            patch.object(pack_nuget, "load") as load,
            patch.object(pack_nuget, "xcframework") as framework,
            self.assertRaisesRegex(ValueError, "win-x64"),
        ):
            pack_nuget.pack(("win-arm64",))
        load.assert_not_called()
        framework.assert_not_called()

    def test_windows_rejects_conflicting_artifact_roots_before_selecting_dotnet(self):
        artifact_roots = {"VM_RUNNER_ARTIFACTS_DIR": "current", "NAVI_CI_ARTIFACTS": "legacy"}
        with (
            patch.dict(os.environ, artifact_roots, clear=True),
            self.assertRaisesRegex(ValueError, "Conflicting VM_RUNNER_ARTIFACTS_DIR"),
        ):
            windows.main([])

    def test_combined_package_carries_all_runtime_assets(self):
        with (
            patch.object(pack_nuget, "xcframework") as framework,
            patch.object(pack_nuget.load("nuget"), "pack_native") as native,
        ):
            pack_nuget.pack(("osx-arm64", "osx-x64", "win-x64"))
        native.assert_called_once()
        self.assertEqual(native.call_args.args, (framework.return_value,))
        self.assertEqual(native.call_args.kwargs["runtimes"], ("osx-arm64", "osx-x64", "win-x64"))
        package_files = native.call_args.kwargs["package_files"]
        self.assertIn("licenses/Rust/LICENSE-MIT", package_files)
        self.assertNotIn("licenses/registry-notices.json", package_files)


if __name__ == "__main__":
    unittest.main()
