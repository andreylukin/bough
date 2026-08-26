.DEFAULT_GOAL := help
.PHONY: help check build test lint gates release audit-plugins

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

release: ## cargo build --release
	cargo build --release

audit-plugins: release ## REQUIREMENTS §17 Phase 8: boot with each bough-base row disabled, assert the tree settles
	./scripts/audit-plugins.sh
