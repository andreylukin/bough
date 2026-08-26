.DEFAULT_GOAL := help
.PHONY: help check build test lint gates release audit-plugins live bench

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
