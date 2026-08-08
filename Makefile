# bough — cargo workspace at the repo root. `make help` lists targets.

SMOKE_PORT ?= 43219

.PHONY: help check build test lint release server tui smoke tui-test gates

help: ## list targets
	@grep -E '^[a-z][a-zA-Z_-]*:.*##' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-12s %s\n", $$1, $$2}'

check: ## cargo check --workspace
	cargo check --workspace

build: ## cargo build --workspace
	cargo build --workspace

test: ## cargo test --workspace
	cargo test --workspace

lint: ## rustfmt check + clippy (warnings as errors)
	cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings

release: ## cargo build --release
	cargo build --release

server: release ## run the server on a scratch BOUGH_HOME (SMOKE_PORT=$(SMOKE_PORT))
	BOUGH_HOME=$$(mktemp -d) BOUGH_PORT=$(SMOKE_PORT) ./target/release/bough start

tui: ## run the TUI against BOUGH_PORT (default live 4321)
	cargo run --release -p bough -- tui

smoke: release ## boot the server + drive the TUI via shell-use; SMOKE_MODEL=openai/gpt-5.6-luna for a live turn
	./scripts/smoke.sh

tui-test: release ## drive the TUI through a real PTY and assert on screen (SMOKE_MODEL adds a live turn)
	./scripts/tui-test.sh

gates: build test ## the pre-commit gates
