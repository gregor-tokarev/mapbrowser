.DEFAULT_GOAL := help

PYTHON ?= python3

# Resolve Cargo's actual output directory, including CARGO_TARGET_DIR overrides.
APP_BUNDLE = $$(cargo metadata --no-deps --format-version=1 | $(PYTHON) -c 'import json, sys; from pathlib import Path; print(Path(json.load(sys.stdin)["target_directory"]) / "bundle" / "Map Browser.app")')

.PHONY: help build run dev test check fmt test-server quit-running

help:
	@printf '%s\n' \
	  'make run          Build the release app and open it' \
	  'make build        Build and sign the release app bundle' \
	  'make dev          Build the debug app and run with terminal logs' \
	  'make test         Run browser and bundle-signing tests' \
	  'make check        Check Rust formatting and all targets' \
	  'make fmt          Format Rust source' \
	  'make test-server  Serve the local browser test pages'

BUNDLE_ID = dev.mapbrowser.browser

build: quit-running
	$(PYTHON) scripts/bundle.py --release

run: build
	open "$(APP_BUNDLE)"

dev: quit-running
	$(PYTHON) scripts/bundle.py
	"$(APP_BUNDLE)/Contents/MacOS/mapbrowser"

# A running copy keeps executing the old binary, and `open` would only bring
# it to the front. Quit it normally (state is saved) and wait up to 10 s.
quit-running:
	@osascript -e 'if application id "$(BUNDLE_ID)" is running then tell application id "$(BUNDLE_ID)" to quit' >/dev/null 2>&1 || true
	@n=0; while pgrep -xq mapbrowser && [ $$n -lt 50 ]; do sleep 0.2; n=$$((n + 1)); done

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
