#!/usr/bin/env python3
"""Run the SDK's independent Git Rust and installed macOS C fixtures."""

from pathlib import Path

from _tooling import load

ROOT = Path(__file__).resolve().parent.parent


def main(argv=None):
    load("installed_consumers").check_installed(
        root=ROOT,
        git_url="https://github.com/naviapps/configuration-storage.git",
        rust_packages=("configuration-storage",),
        framework=(
            "Artifacts/CConfigurationStorage.xcframework/macos-arm64_x86_64/"
            "CConfigurationStorage.framework"
        ),
        header="configuration_storage.h",
        argv=argv,
    )


if __name__ == "__main__":
    main()
