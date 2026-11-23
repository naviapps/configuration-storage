CARGO ?= cargo
SWIFT ?= swift
SDK_TOOLING ?= mise run sdk-tooling --
SWIFT_FLAGS := -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors

.DEFAULT_GOAL := check
.PHONY: help check check-source test-managed check-distribution check-fast check-rust check-swift acceptance native native-verify lint lint-rust test test-rust test-fixtures test-consumer test-installed-consumers test-c test-bindings docs

help:
	@echo 'check          Source lint, Rust/Swift/managed tests and external Swift consumer (default)'
	@echo 'check-distribution  Tooling tests, native receipt verification, C/.NET/NuGet and installed Rust/C consumers'
	@echo 'check-fast     Lint and compile Rust/Swift tests without running them'
	@echo 'check-rust     Compile Rust library and test targets; no test execution'
	@echo 'check-swift    Compile Swift library and test targets; no test execution'
	@echo 'test-rust      Run Rust workspace tests and doctests, locked and offline'
	@echo 'test-fixtures     Run SDK-owned build and fixture policy tests'
	@echo 'test-consumer  Run the external SwiftPM consumer'
	@echo 'test-installed-consumers  Run standalone Git Rust and installed macOS C clients'
	@echo 'docs           Validate DocC against the owned public Swift symbols'
	@echo 'test           Run strict Swift tests'
	@echo 'test-c         Verify native receipt and run the C ABI consumer'
	@echo 'test-bindings  Verify native receipt; run C/.NET tests and local NuGet consumer (.NET 10 required)'
	@echo 'acceptance     All local macOS gates: Swift, Rust fmt/Clippy/tests, C/.NET/package consumer'
	@echo 'native-verify  Complete source/artifact receipt, notices, exports and platform verification'

# Verification consumes the parent's artifact; it never builds one implicitly.
check: check-source test test-consumer

check-source: lint lint-rust test-rust test-managed

check-distribution: native-verify test-fixtures test-bindings test-installed-consumers

# Iteration only: the complete runtime gate remains explicit below.
check-fast: lint check-rust check-swift

check-rust:
	"$(CARGO)" check --workspace --all-targets --locked --offline

check-swift:
	"$(SWIFT)" build --build-tests $(SWIFT_FLAGS)

# Uses the existing artifact; native rebuilds and Windows acceptance belong to the parent.
acceptance: check check-distribution

native:
	$(SDK_TOOLING) native

native-verify:
	$(SDK_TOOLING) native --verify

lint:
	$(SDK_TOOLING) quality check --dotnet

lint-rust:
	"$(CARGO)" clippy --workspace --all-targets --locked --offline -- -D warnings

test:
	"$(SWIFT)" test $(SWIFT_FLAGS)

test-consumer:
	$(SDK_TOOLING) swift-consumer

test-installed-consumers:
	$(SDK_TOOLING) installed-consumers

test-rust:
	"$(CARGO)" test --workspace --locked --offline

test-c: native-verify
	$(SDK_TOOLING) python -m fixtures.distribution.bindings --c-only

test-bindings: native-verify
	$(SDK_TOOLING) python -m fixtures.distribution.bindings

test-fixtures:
	$(SDK_TOOLING) python -m unittest discover -s fixtures/distribution/tests -v

docs:
	$(SDK_TOOLING) docs

.PHONY: format
format:
	$(SDK_TOOLING) quality format --dotnet

test-managed:
	dotnet run --configuration Release --project bindings/dotnet/tests/ConfigurationStorage.Tests/ConfigurationStorage.Tests.csproj
