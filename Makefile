PYTHON ?= python3
CARGO ?= cargo
SWIFT ?= swift
SWIFT_FLAGS := -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors

.DEFAULT_GOAL := check
.PHONY: help check check-fast check-rust check-swift acceptance native native-verify lint lint-rust test test-rust test-tools test-consumer test-installed-consumers test-c test-bindings docs

help:
	@echo 'check          Source format/lint, Rust Clippy/tests, native receipt verification, tool/Swift tests and external consumer (default)'
	@echo 'check-fast     Lint and compile Rust/Swift tests without running them; verifies native receipt'
	@echo 'check-rust     Compile Rust library and test targets; no test execution'
	@echo 'check-swift    Compile Swift library and test targets; verifies native receipt; no test execution'
	@echo 'test-rust      Run Rust workspace tests and doctests, locked and offline'
	@echo 'test-tools     Run SDK-owned build and fixture policy tests'
	@echo 'test-consumer  Verify native receipt and run a clean external SwiftPM consumer'
	@echo 'test-installed-consumers  Run standalone Git Rust and installed macOS C clients'
	@echo 'docs           Validate DocC against the owned public Swift symbols'
	@echo 'test           Verify native receipt and run strict Swift tests'
	@echo 'test-c         Verify native receipt and run the C ABI consumer'
	@echo 'test-bindings  Verify native receipt; run C/.NET tests and local NuGet consumer (.NET 10 required)'
	@echo 'acceptance     All local macOS gates: Swift, Rust fmt/Clippy/tests, C/.NET/package consumer'
	@echo 'native-verify  Complete source/artifact receipt, notices, exports and platform verification'

# Verification consumes the parent's artifact; it never builds one implicitly.
check: lint lint-rust test-rust native-verify test-tools test test-consumer

# Iteration only: the complete runtime gate remains explicit below.
check-fast: lint check-rust check-swift

check-rust:
	"$(CARGO)" check --workspace --all-targets --locked --offline

check-swift: native-verify
	"$(SWIFT)" build --build-tests $(SWIFT_FLAGS)

# Uses the existing artifact; native rebuilds and Windows acceptance belong to the parent.
acceptance: check test-bindings test-installed-consumers

native:
	"$(PYTHON)" tools/native.py

native-verify:
	"$(PYTHON)" tools/native.py --verify

lint:
	mise exec -- python3 -B tools/quality.py check --dotnet

lint-rust:
	"$(CARGO)" fmt --all -- --check
	"$(CARGO)" clippy --workspace --all-targets --locked --offline -- -D warnings

test: native-verify
	"$(SWIFT)" test $(SWIFT_FLAGS)

test-consumer: native-verify
	"$(PYTHON)" -B tools/consumer.py

test-installed-consumers:
	"$(PYTHON)" -B tools/install-consumers.py

test-rust:
	"$(CARGO)" test --workspace --locked --offline

test-c: native-verify
	"$(PYTHON)" tools/bindings.py --c-only

test-bindings: native-verify
	"$(PYTHON)" tools/bindings.py

test-tools:
	"$(PYTHON)" -B -m unittest discover -s tools/tests -v

docs:
	"$(PYTHON)" -B tools/docs.py

.PHONY: format
format:
	mise exec -- python3 -B tools/quality.py format --dotnet
