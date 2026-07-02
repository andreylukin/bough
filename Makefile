# bough (Deno). Root shorthands for the deno tasks.

.PHONY: serve check test

serve: ## Run the server on 127.0.0.1:4321
	deno task dev

check: ## Typecheck
	deno task check

test: ## Run tests
	deno task test
