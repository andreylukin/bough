.DEFAULT_GOAL := help
.PHONY: help check build test lint gates release audit-plugins live bench tui-test

help: ## list targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-16s %s\n", $$1, $$2}'

check: ## cargo check --workspace --all-targets
	cargo check --workspace --all-targets

build: ## cargo build --workspace --all-targets
	cargo build --workspace --all-targets

test: ## cargo test --workspace (offline, hermetic)
	cargo test --workspace

lint: ## rustfmt check + clippy (warnings as errors)
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

gates: build lint test ## the pre-commit gates

# REQUIREMENTS §17 / P2-D27: `make test` is OFFLINE and hermetic, always. The live set is
# `#[ignore]`d and gated on BOUGH_LIVE=1; this is the target that runs it, with the key sourced
# from ~/.bough/env and never echoed.
live: ## run the #[ignore]d live model tests (needs ~/.bough/env with ANTHROPIC_API_KEY)
	@test -f $(HOME)/.bough/env || { echo "make live: $(HOME)/.bough/env not found"; exit 1; }
	@set -a; . $(HOME)/.bough/env; set +a; \
	 test -n "$$ANTHROPIC_API_KEY" || { echo "make live: ANTHROPIC_API_KEY is not set in $(HOME)/.bough/env"; exit 1; }; \
	 BOUGH_LIVE=1 cargo test --workspace --all-targets -- --ignored --nocapture

# P2-D24: the number that decides Phase 0's open item 1 (the fiber poll loop). Recorded in BUILD.md.
bench: ## run the #[ignore]d measurements (offline)
	BOUGH_BENCH=1 cargo test --workspace --all-targets -- --ignored --nocapture bench

release: ## cargo build --release
	cargo build --release

audit-plugins: release ## REQUIREMENTS §17 Phase 8: boot with each bough-base row disabled, assert the tree settles
	./scripts/audit-plugins.sh

# REQUIREMENTS §17 Phase 3: the shell-use suite. It drives the RELEASE binary in a real PTY, with
# `$$BOUGH_HOME` pointed at a scratch directory and a generated patch that swaps `llm.anthropic`
# for `llm-replay` — so the offline half needs no network and no key.
#
# When `~/.bough/env` carries an ANTHROPIC_API_KEY the whole suite runs a SECOND time with
# `BOUGH_LIVE=1` and NO replay patch: the scripts then assert a real streamed haiku answer lands
# in the focus pane. The key is sourced, never echoed.
TUI_SCRATCH := $(CURDIR)/target/tui-test
TUI_PATCH   := $(TUI_SCRATCH)/llm-replay.patch.yml

tui-test: release ## REQUIREMENTS §17 Phase 3: drive the release binary through scripts/tui/*.sh
	@command -v shell-use >/dev/null || { echo "make tui-test: shell-use is not on PATH"; exit 1; }
	@command -v sqlite3 >/dev/null || { echo "make tui-test: sqlite3 is not on PATH"; exit 1; }
	@rm -rf $(TUI_SCRATCH); mkdir -p $(TUI_SCRATCH) $(TUI_SCRATCH)/warm
	@cp scripts/tui/fixtures/llm-replay.patch.yml $(TUI_PATCH)
	@# Warm the binary once before any script drives it. On macOS the FIRST exec of a freshly
	@# written binary can fail silently, and the script that hit it reported a dead screen rather
	@# than the boot failure it is. `--check` boots, quiesces and tears down, so it also fails
	@# loudly here if the composed tree is broken.
	@BOUGH_HOME=$(TUI_SCRATCH)/warm $(CURDIR)/target/release/bough --check >/dev/null 2>&1 || true
	@echo "== tui-test: replay half =="
	@set -e; for s in scripts/tui/[0-9]*.sh; do \
	   echo "# $$s"; \
	   BOUGH_BIN=$(CURDIR)/target/release/bough \
	   BOUGH_HOME=$(TUI_SCRATCH)/replay \
	   BOUGH_PATCH=$(TUI_PATCH) \
	   BOUGH_LIVE= \
	   bash $$s; \
	 done
	@if [ -f $(HOME)/.bough/env ]; then \
	   set -a; . $(HOME)/.bough/env; set +a; \
	   if [ -n "$$ANTHROPIC_API_KEY" ]; then \
	     echo "== tui-test: live half (haiku) =="; \
	     set -e; for s in scripts/tui/[0-9]*.sh; do \
	       echo "# $$s (live)"; \
	       BOUGH_BIN=$(CURDIR)/target/release/bough \
	       BOUGH_HOME=$(TUI_SCRATCH)/live \
	       BOUGH_PATCH= \
	       BOUGH_LIVE=1 \
	       ANTHROPIC_API_KEY="$$ANTHROPIC_API_KEY" \
	       bash $$s; \
	     done; \
	   else \
	     echo "== tui-test: no ANTHROPIC_API_KEY in $(HOME)/.bough/env; skipping the live half =="; \
	   fi; \
	 else \
	   echo "== tui-test: no $(HOME)/.bough/env; skipping the live half =="; \
	 fi
