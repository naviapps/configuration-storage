CARGO ?= cargo
SWIFT ?= swift
SDK_TOOLING ?= mise run sdk-tooling --
SWIFT_FLAGS := -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors
CBINDGEN := cbindgen --config crates/ffi/cbindgen.toml crates/ffi/src/lib.rs
C_HEADER := crates/ffi/include/configuration_storage.h

.DEFAULT_GOAL := check
.PHONY: help check check-source test-managed check-distribution check-fast check-rust check-swift acceptance native native-verify check-header generate-header lint lint-rust test test-rust test-distribution-tools test-consumer test-installed-consumers test-c test-bindings clean

help:
	@echo 'check          Source lint, Rust/Swift/managed tests and external Swift consumer (default)'
	@echo 'check-distribution  Tooling tests, native receipt verification, C/.NET/NuGet and installed Rust/C consumers'
	@echo 'check-fast     Lint and compile Rust/Swift tests without running them'
	@echo 'check-rust     Compile Rust library and test targets; no test execution'
	@echo 'check-swift    Compile Swift library and test targets; no test execution'
	@echo 'test-rust      Run Rust workspace tests and doctests, locked and offline'
	@echo 'test-distribution-tools     Run SDK-owned build and fixture policy tests'
	@echo 'test-consumer  Run the external SwiftPM consumer'
	@echo 'test-installed-consumers  Run standalone Git Rust and installed macOS C clients'
	@echo 'test           Run strict Swift tests'
	@echo 'test-c         Verify native receipt and run the C ABI consumer'
	@echo 'test-bindings  Verify native receipt; run C/.NET tests and local NuGet consumer (.NET 10 required)'
	@echo 'acceptance     All local macOS gates: Swift, Rust fmt/Clippy/tests, C/.NET/package consumer'
	@echo 'native-verify  Complete source/artifact receipt, notices, exports and platform verification'
	@echo 'clean          Remove checkout-local build outputs'

# Verification consumes the parent's artifact; it never builds one implicitly.
check: check-source test test-consumer

check-source: lint check-header lint-rust test-rust test-managed

check-distribution: native-verify test-distribution-tools test-bindings test-installed-consumers

# Iteration only: the complete runtime gate remains explicit below.
check-fast: lint check-rust check-swift

check-rust:
	"$(CARGO)" check --workspace --all-targets --locked --offline

check-swift:
	"$(SWIFT)" build --build-tests $(SWIFT_FLAGS)

# Uses the existing artifact; native rebuilds and Windows acceptance belong to the parent.
acceptance: check check-distribution

native: check-header
	$(SDK_TOOLING) xcframework

native-verify: check-header
	$(SDK_TOOLING) xcframework --verify

check-header:
	bash -o pipefail -c '$(CBINDGEN) | diff -u "$(C_HEADER)" -'

generate-header:
	$(CBINDGEN) --output "$(C_HEADER)"

lint:
	$(SDK_TOOLING) quality check --dotnet

lint-rust:
	"$(CARGO)" clippy --workspace --all-targets --locked --offline -- -D warnings

test:
	"$(SWIFT)" test $(SWIFT_FLAGS)

test-consumer:
	$(SDK_TOOLING) swift-consumer

test-installed-consumers:
	$(SDK_TOOLING) rust-c-consumers

test-rust:
	"$(CARGO)" test --workspace --locked --offline

test-c: native-verify
	$(SDK_TOOLING) python -m distribution.verify_bindings --c-only

test-bindings: native-verify
	$(SDK_TOOLING) python -m distribution.verify_bindings

.PHONY: test-native-bindings
test-native-bindings: native-verify
	$(SDK_TOOLING) python -m distribution.verify_bindings --no-package

test-distribution-tools:
	$(SDK_TOOLING) python -m unittest discover -s distribution/tests -v

.PHONY: format
format:
	$(SDK_TOOLING) quality format --dotnet

test-managed:
	dotnet run --configuration Release --project bindings/dotnet/tests/ConfigurationStorage.Tests/ConfigurationStorage.Tests.csproj

clean:
	$(SDK_TOOLING) clean target .build .native-build build \
		bindings/dotnet/src/NaviApps.ConfigurationStorage/bin \
		bindings/dotnet/src/NaviApps.ConfigurationStorage/obj \
		bindings/dotnet/tests/ConfigurationStorage.Tests/bin \
		bindings/dotnet/tests/ConfigurationStorage.Tests/obj
