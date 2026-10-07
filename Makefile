.DEFAULT_GOAL := help

PYTHON ?= python3

# Resolve Cargo's actual output directory, including CARGO_TARGET_DIR overrides.
APP_BUNDLE = $$(cargo metadata --no-deps --format-version=1 | $(PYTHON) -c 'import json, sys; from pathlib import Path; print(Path(json.load(sys.stdin)["target_directory"]) / "bundle" / "Mapbrowser.app")')

.PHONY: help build run dev test check fmt test-server

help:
	@printf '%s\n' \
	  'make run          Build the release app and open it' \
	  'make build        Build and sign the release app bundle' \
	  'make dev          Build the debug app and run with terminal logs' \
	  'make test         Run browser and bundle-signing tests' \
	  'make check        Check Rust formatting and all targets' \
	  'make fmt          Format Rust source' \
	  'make test-server  Serve the local browser test pages'

build:
	$(PYTHON) scripts/bundle.py --release

run: build
	open "$(APP_BUNDLE)"

dev:
	$(PYTHON) scripts/bundle.py
	"$(APP_BUNDLE)/Contents/MacOS/mapbrowser"

test:
	cargo test --bin mapbrowser
	$(PYTHON) -m unittest discover -s scripts -p 'test_bundle.py'
	$(PYTHON) -m unittest discover -s scripts -p 'test_analyze_memory.py'

check:
	cargo fmt --check
	cargo check --all-targets

fmt:
	cargo fmt

test-server:
	$(PYTHON) scripts/test_server.py
