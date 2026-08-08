# bough — cargo workspace at the repo root. `make help` lists targets.

SMOKE_PORT ?= 43219

# The dev profile: this checkout's own data root and port, so running it never
# touches the install at ~/.bough:4321. Stable (not a mktemp), so dev sessions
# survive between runs. Gitignored.
DEV_HOME ?= $(CURDIR)/.dev
DEV_PORT ?= 4322
DEV_ENV   = BOUGH_HOME=$(DEV_HOME) BOUGH_PORT=$(DEV_PORT)

.PHONY: help check build test lint release dev dev-server dev-stop dev-logs server tui smoke tui-test gates

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

# ---- running this checkout --------------------------------------------------
# One command. The wrapper starts the dev server detached if it is down, so this
# is the whole story: `make dev`.

dev: release ## run THIS checkout — TUI + server on the dev profile (.dev/, port 4322)
	@$(DEV_ENV) ./scripts/bough

dev-server: release ## just the dev server, in the foreground (logs to the terminal)
	@$(DEV_ENV) ./scripts/bough run

dev-stop: ## stop the dev server (never touches the install at ~/.bough)
	@$(DEV_ENV) ./scripts/bough kill

dev-logs: ## tail the dev server's log
	@tail -f $(DEV_HOME)/server.log

server: release ## the server alone on a throwaway BOUGH_HOME (SMOKE_PORT)
	BOUGH_HOME=$$(mktemp -d) BOUGH_PORT=$(SMOKE_PORT) ./target/release/bough start

tui: release ## the TUI against SMOKE_PORT — the companion to `make server`
	BOUGH_PORT=$(SMOKE_PORT) ./target/release/bough tui

smoke: release ## boot the server + drive the TUI via shell-use; SMOKE_MODEL=openai/gpt-5.6-luna for a live turn
	./scripts/smoke.sh

tui-test: release ## drive the TUI through a real PTY and assert on screen (SMOKE_MODEL adds a live turn)
	./scripts/tui-test.sh

gates: build test ## the pre-commit gates
