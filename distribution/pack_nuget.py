#!/usr/bin/env python3
"""Package verified ConfigurationStorage native assets."""

import argparse
from pathlib import Path

from sdk_tooling.adapters import load
from sdk_tooling.native_plan import ffi_product
from sdk_tooling.windows_delivery import RID
from sdk_tooling.xcframework import xcframework

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "NaviApps.ConfigurationStorage"
PROJECT = ROOT / "bindings/dotnet/src" / PACKAGE / (PACKAGE + ".csproj")
NUGET_CONFIG = ROOT / "bindings/dotnet/tests/ConfigurationStorage.Tests/NuGet.Config"
RUNTIMES = ("osx-arm64", "osx-x64", RID)


def pack(runtimes, *, environment=None):
    if not runtimes or set(runtimes) - set(RUNTIMES):
        raise ValueError("ConfigurationStorage supports osx-arm64, osx-x64 and win-x64")
    native = ffi_product(ROOT) if set(runtimes) == {RID} else xcframework(ROOT)
    files = {"LICENSE": ROOT / "LICENSE", "README.md": ROOT / "bindings/dotnet/README.md"}
    files.update(
        {
            "licenses/" + path.relative_to(ROOT / "ThirdParty").as_posix(): path
            for path in load("artifacts").files(ROOT / "ThirdParty")
            if path.name not in (".DS_Store", "registry-notices.json")
        }
    )
    return load("nuget").pack_native(
        native,
        project=PROJECT,
        package_files=files,
        nuget_config=NUGET_CONFIG,
        runtimes=runtimes,
        environment=environment,
        input_files=[Path(__file__), ROOT / "native.toml", ROOT / "mise.toml"],
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--runtime", action="append", required=True, choices=RUNTIMES)
    parser.add_argument("--result", type=Path, help="Write the verified package filename")
    args = parser.parse_args(argv)
    package = pack(args.runtime)
    if args.result is not None:
        with args.result.open("x", encoding="utf-8") as output:
            output.write(package.name)


if __name__ == "__main__":
    main()
