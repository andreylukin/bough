# bough monorepo. Gleam has no native workspace, so we drive each package.
# Packages are wired via `path` dependencies (server -> core).

PACKAGES := bough_core bough_server

.PHONY: check test build sidecar clean serve $(addprefix check-,$(PACKAGES))

check: ## Type-check every package
	@for p in $(PACKAGES); do echo "== check $$p =="; (cd packages/$$p && gleam check) || exit 1; done

build: sidecar ## Compile every package (+ the code-mode sidecar)
	@for p in $(PACKAGES); do echo "== build $$p =="; (cd packages/$$p && gleam build) || exit 1; done

# The bough-monty code-mode sidecar (SPEC §5.2). Skipped without cargo or with
# BOUGH_NO_MONTY=1, so a Gleam-only checkout still builds; `bough update` keeps
# the binary fresh by symlinking it into ~/.bough/bin.
sidecar: ## Build the bough-monty code-mode sidecar (Rust)
	@if [ "$${BOUGH_NO_MONTY:-0}" = "1" ] || ! command -v cargo >/dev/null 2>&1; then \
		echo "== skip sidecar (no cargo or BOUGH_NO_MONTY=1) =="; \
	else \
		echo "== build sidecar =="; \
		cargo build --release --manifest-path sidecar/Cargo.toml || exit 1; \
		mkdir -p "$$HOME/.bough/bin"; \
		ln -sf "$$PWD/sidecar/target/release/bough-monty" "$$HOME/.bough/bin/bough-monty"; \
	fi

test: ## Run tests for every package
	@for p in $(PACKAGES); do echo "== test $$p =="; (cd packages/$$p && gleam test) || exit 1; done

clean: ## Remove build artifacts
	@for p in $(PACKAGES); do rm -rf packages/$$p/build; done

# Run the server on 127.0.0.1:4096 (SPEC.md §10)
serve:
	cd packages/bough_server && gleam run
