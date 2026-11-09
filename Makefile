# Everyday tasks. `make` lists them; `make ci` runs what the GitHub workflow runs.

SHELL := /bin/bash
.DEFAULT_GOAL := help

# rustup's tools must win over any system Rust (which has no wasm target).
export PATH := $(HOME)/.cargo/bin:$(PATH)

ENGINE := engine/Cargo.toml
# Template container apps (wasm32-wasip1). Native engine builds embed them (the `bundled-apps`
# feature); the browser fetches them from an app registry.
APPS := apps/Cargo.toml
WASM_OUT := frontend/src/lib/wasm
WASM := $(WASM_OUT)/engine_bg.wasm
ENGINE_SOURCES := $(shell find engine/src -name '*.rs') engine/build.rs engine/Cargo.toml engine/Cargo.lock
# Scenarios `make scenario-check` checks; override with SCENARIO=path/to/scenario.json.
SCENARIO ?= $(wildcard scenarios/*/scenario.json)

.PHONY: help setup doctor wasm apps registry scenario-check fmt lint test check build dev e2e oracle-goldens fidelity ci clean

help: ## Show this help
	@grep -hE '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk -F ':.*## ' '{printf "  \033[33m%-8s\033[0m %s\n", $$1, $$2}'

setup: ## Install wasm-pack (if missing) and the frontend dependencies
	@command -v wasm-pack >/dev/null || cargo install wasm-pack
	cd frontend && bun install
	@echo "For the browser tests also run: cd frontend && bunx playwright install chromium"

doctor: ## Check the toolchain and say what to fix
	@./scripts/doctor.sh

# Rebuilt only when the engine's sources change. No apps inside: the browser fetches them.
$(WASM): $(ENGINE_SOURCES)
	wasm-pack build engine --target web --out-dir ../$(WASM_OUT) --no-default-features

wasm: $(WASM) ## Build the engine to WASM into the frontend

apps: ## Build the template apps to wasm32-wasip1
	cargo build --manifest-path $(APPS) --release --target wasm32-wasip1

scenario-check: ## Check scenarios (and play their solution.json); SCENARIO=path to pick one
	cargo run -q --manifest-path $(ENGINE) --bin scenario-check -- $(SCENARIO)

registry: apps ## Check the scenarios and build the local registry into frontend/static/registry
	cargo run -q --manifest-path $(ENGINE) --bin scenario-check -- $(wildcard scenarios/*/scenario.json)
	bun scripts/build-registry.ts

fmt: ## Format the Rust code
	cargo fmt --manifest-path $(ENGINE)
	cargo fmt --all --manifest-path $(APPS)

lint: ## Check formatting and run clippy with warnings as errors
	cargo fmt --manifest-path $(ENGINE) --check
	cargo fmt --all --manifest-path $(APPS) --check
	cargo clippy --manifest-path $(ENGINE) --all-targets -- -D warnings
	cargo clippy --manifest-path $(APPS) --all-targets -- -D warnings

test: ## Run the engine, template app and frontend unit tests
	cargo test --manifest-path $(ENGINE)
	cargo test --manifest-path $(APPS)
	cd frontend && bun run test

check: $(WASM) registry ## Type-check the frontend
	cd frontend && bun run check

build: $(WASM) registry ## Production build of the frontend
	cd frontend && bun run build

dev: $(WASM) registry ## Start the dev server
	cd frontend && bun run dev

e2e: $(WASM) registry ## Play the game in a real browser (Playwright)
	cd frontend && bun run test:e2e

# Upstream fidelity: `oracle/` (Go, never shipped) runs upstream Kubernetes on the cases in
# engine/tests/fidelity and writes the expected outputs the engine is tested against.
FIDELITY := engine/tests/fidelity
# Generated from upstream's Go types; checked in so the engine builds without Go.
GOTYPES := engine/src/gotypes.rs
GENERATED := $(FIDELITY) $(GOTYPES)

oracle-goldens: ## Regenerate the upstream fidelity goldens (needs Go)
	@command -v go >/dev/null || { echo "oracle-goldens needs Go (version in oracle/go.mod)"; exit 1; }
	cd oracle && go vet ./... && go run ./cmd/kubeoracle goldens ../$(FIDELITY)
	cd oracle && go run ./cmd/kubeoracle gotypes ../$(GOTYPES)

fidelity: oracle-goldens ## Fail if the checked-in fidelity goldens are not what upstream produces
	@git diff --exit-code -- $(GENERATED) || { echo "goldens are stale: run make oracle-goldens and review the diff"; exit 1; }
	@test -z "$$(git status --porcelain --untracked-files=all -- $(GENERATED))" || { git status --short -- $(GENERATED); echo "untracked goldens: git add them"; exit 1; }

ci: lint test fidelity check build e2e ## Everything CI runs

clean: ## Remove build outputs
	cargo clean --manifest-path $(ENGINE)
	cargo clean --manifest-path $(APPS)
	rm -rf $(WASM_OUT) frontend/static/registry frontend/build frontend/.svelte-kit frontend/test-results
