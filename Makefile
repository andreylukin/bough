# bough — the product lives in bough-next/ (Deno). These are root shorthands.

.PHONY: serve check test

serve: ## Run the server on 127.0.0.1:4321
	cd bough-next && deno task dev

check: ## Typecheck
	cd bough-next && deno task check

test: ## Run tests
	cd bough-next && deno task test
