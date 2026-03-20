SHELL := /bin/bash

CARGO := source ~/.cargo/env >/dev/null 2>&1; cargo
TAURI_DIR := src-tauri

test-all:
	pnpm test
	cd $(TAURI_DIR) && $(CARGO) test --workspace

lint-all:
	pnpm lint

build-all:
	pnpm build
	cd $(TAURI_DIR) && $(CARGO) build --workspace
