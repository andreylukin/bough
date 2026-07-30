# bough (Bun). Root shorthands for the package.json scripts.

.PHONY: serve check test

serve: ## Run the server on 127.0.0.1:4321
	bun run dev

check: ## Typecheck
	bun run check

test: ## Run tests
	bun test
	bun test ./ahe   # bunfig pins the runner's root to ./src (see bunfig.toml)
