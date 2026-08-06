# bough — TS system (bun, from root) + bough-rs Rust rewrite (cargo, in bough-rs/)
# `make help` lists targets.

RS_DIR := bough-rs
SMOKE_PORT ?= 43219

.PHONY: help check test dev tui gates \
        rs-check rs-build rs-test rs-lint rs-release rs-server rs-tui rs-smoke rs-gates all

help: ## list targets
	@grep -E '^[a-z][a-zA-Z_-]*:.*##' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-12s %s\n", $$1, $$2}'

# ---- TS system ----
check: ## typecheck (tsc --noEmit)
	bun run check

test: ## bun unit + integration suite
	bun test

dev: ## TS server with --watch
	bun run dev

tui: ## TS TUI against the local server
	bun run tui

gates: check test ## the TS pre-commit gates

# ---- bough-rs ----
rs-check: ## cargo check --workspace
	cd $(RS_DIR) && cargo check --workspace

rs-build: ## cargo build --workspace
	cd $(RS_DIR) && cargo build --workspace

rs-test: ## cargo test --workspace
	cd $(RS_DIR) && cargo test --workspace

rs-lint: ## rustfmt check + clippy (warnings as errors)
	cd $(RS_DIR) && cargo fmt --check && cargo clippy --workspace -- -D warnings

rs-release: ## cargo build --release
	cd $(RS_DIR) && cargo build --release

rs-server: rs-release ## run the Rust server on a scratch BOUGH_HOME (SMOKE_PORT=$(SMOKE_PORT))
	BOUGH_HOME=$$(mktemp -d) BOUGH_PORT=$(SMOKE_PORT) $(RS_DIR)/target/release/bough start

rs-tui: ## run the Rust TUI against BOUGH_PORT (default live 4321)
	cd $(RS_DIR) && cargo run --release -p bough -- tui

rs-smoke: rs-release ## boot Rust server + drive the TUI via shell-use; SMOKE_MODEL=openai/gpt-5.6-luna for a live turn
	$(RS_DIR)/smoke.sh

rs-gates: rs-build rs-test ## the Rust pre-commit gates

all: gates rs-gates ## everything
