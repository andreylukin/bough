# bough monorepo. Gleam has no native workspace, so we drive each package.
# Packages are wired via `path` dependencies (server + tui -> core).

PACKAGES := bough_core bough_server bough_tui

.PHONY: check test build clean serve $(addprefix check-,$(PACKAGES))

check: ## Type-check every package
	@for p in $(PACKAGES); do echo "== check $$p =="; (cd packages/$$p && gleam check) || exit 1; done

build: ## Compile every package
	@for p in $(PACKAGES); do echo "== build $$p =="; (cd packages/$$p && gleam build) || exit 1; done

test: ## Run tests for every package
	@for p in $(PACKAGES); do echo "== test $$p =="; (cd packages/$$p && gleam test) || exit 1; done

clean: ## Remove build artifacts
	@for p in $(PACKAGES); do rm -rf packages/$$p/build; done

# Run the server on 127.0.0.1:4096 (SPEC.md §10)
serve:
	cd packages/bough_server && gleam run
