.DEFAULT_GOAL := help
.PHONY: help check build test doc-test lint gate-crate gates release audit-plugins live bench bench-tools tui-test tui-test-replay ux2

help: ## list targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-16s %s\n", $$1, $$2}'

check: ## cargo check --workspace --all-targets
	cargo check --workspace --all-targets

build: ## cargo build --workspace --all-targets
	cargo build --workspace --all-targets

# nextest: one process per test, every binary's tests scheduled across all cores at once
# (`cargo test` runs test binaries one after another). Doctests are cargo's alone, so they run
# separately. Each crate's tests/*.rs are ONE target (tests/main.rs, `autotests = false`);
# `scripts/check-test-mods.sh` (in `lint`) fails when a test file is not declared there.
test: ## nextest --workspace + doctests (offline, hermetic)
	cargo nextest run --workspace --no-run
	@# Warm the freshly linked binary ONCE before any test execs it: macOS scans a new executable
	@# on its first launch (XProtect), and inside a gate every test binary was just relinked, so
	@# the scan storm can push the first `bough` boot past a test's own 30 s deadline
	@# (`boot::sigint_tears_down_before_exit`, seen twice in gates, never in isolation). The real
	@# cure is the Developer Tools exemption for the terminal; this makes the gate honest without it.
	@./target/debug/bough --version >/dev/null 2>&1 || true
	cargo nextest run --workspace
	cargo test --workspace --doc

doc-test: ## doctests only
	cargo test --workspace --doc

# Per-work-package verification: only the crates you touched. The full `make gates` runs at
# Integrate and at close, not after every work package.
gate-crate: ## CRATES="bough-plugin-x bough-plugin-y" — nextest + clippy for those crates only
	@test -n "$(CRATES)" || { echo "make gate-crate CRATES=\"bough-plugin-a bough-plugin-b\""; exit 1; }
	cargo nextest run $(foreach c,$(CRATES),-p $(c))
	cargo clippy $(foreach c,$(CRATES),-p $(c)) --all-targets -- -D warnings

lint: ## rustfmt check + clippy (warnings as errors) + test-target check
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	./scripts/check-test-mods.sh

# `tui-test-replay` is IN the gates on purpose: every screen-level bullet of the interface-cutover
# phase (V1-V8 and SWAP) lives in `scripts/tui/*.sh`, so a gate without them exercises none of the
# interface it gates. The REPLAY half only — `make test` and `make gates` are offline and hermetic
# (AGENTS.md); the live half is `make tui-test`.
# No `build` step: `test` builds exactly what it runs and `lint` shares its metadata.
gates: lint test tui-test-replay ## the pre-commit gates

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

# Phase codemode V9: the two consumers of the `tools` seam over ONE task bank. NOT in `make gates`
# — it drives the release binary over ~15 tasks twice and is a measurement Andrey decides on, not a
# regression test. `BOUGH_LIVE=1 make bench-tools` runs the same bank on live haiku for both arms.
bench-tools: release ## phase codemode: typed tools vs code mode over the task bank
	BOUGH_BENCH=1 BOUGH_BIN=$(CURDIR)/target/release/bough \
	  cargo test -p bough-bench-tools -- --ignored --nocapture

release: ## cargo build --release
	cargo build --release

audit-plugins: release ## REQUIREMENTS §17 Phase 8: boot with each bough-base row disabled, assert the tree settles
	./scripts/audit-plugins.sh

# REQUIREMENTS §17 Phase 3 (and Phase 6: 27-drafts, 28-mcp-tool, 29-swap-collector,
# 30-swap-wards). Both halves glob `scripts/tui/[0-9]*.sh`, so a new script is wired in by
# existing there. The shell-use suite. It drives the RELEASE binary in a real PTY, with
# `$$BOUGH_HOME` pointed at a scratch directory and a generated patch that swaps `llm.anthropic`
# for `llm-replay` — so the offline half needs no network and no key.
#
# When `~/.bough/env` carries an ANTHROPIC_API_KEY the whole suite runs a SECOND time with
# `BOUGH_LIVE=1` and NO replay patch: the scripts then assert a real streamed haiku answer lands
# in the focus pane. The key is sourced, never echoed.
TUI_SCRATCH := $(CURDIR)/target/tui-test
TUI_PATCH   := $(TUI_SCRATCH)/llm-replay.patch.yml

# The suite boots the RELEASE binary. It was tried on the debug one (to spare the gate a second
# compile): three runs, three different timing misses (a swallowed catch-up, a boot that lost the
# factory race, a /seal that never sealed). The scripts' waits are tuned against the optimized
# binary; an incremental release rebuild is seconds next to a 39-minute suite.
tui-test-replay: release ## the OFFLINE half of the shell-use suite (in `make gates`)
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
	@# The code-mode arm of the consumer-parameterised scripts. The loop above runs them with
	@# BOUGH_CONSUMER unset, which is the TYPED control arm; without this pass the program row is
	@# only ever asserted absent.
	@echo "== tui-test: replay half, code-mode arm =="
	@set -e; for s in scripts/tui/31-program.sh; do \
	   echo "# $$s (codemode)"; \
	   BOUGH_BIN=$(CURDIR)/target/release/bough \
	   BOUGH_HOME=$(TUI_SCRATCH)/replay-codemode \
	   BOUGH_PATCH=$(TUI_PATCH) \
	   BOUGH_LIVE= \
	   BOUGH_CONSUMER=codemode \
	   bash $$s; \
	 done

tui-test: tui-test-replay ## REQUIREMENTS §17 Phase 3: the whole suite, replay half then live half
	@if [ -f $(HOME)/.bough/env ] || [ -n "$$ANTHROPIC_API_KEY" ]; then \
	   if [ -f $(HOME)/.bough/env ]; then set -a; . $(HOME)/.bough/env; set +a; fi; \
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
	     echo "== tui-test: no ANTHROPIC_API_KEY in the environment or $(HOME)/.bough/env; skipping the live half =="; \
	   fi; \
	 else \
	   echo "== tui-test: no ANTHROPIC_API_KEY and no $(HOME)/.bough/env; skipping the live half =="; \
	 fi

# phase ux1 §3 V11: the UX re-audit. Three personas re-walk the top twelve findings of
# `docs/ux-audit-1.md` against the RELEASE binary, live haiku for both tiers, capturing an SVG per
# step into `docs/ux-audit-2-shots/<persona>/`. It exits non-zero if any blocker or major verdict
# is not `fixed` — those are the rows of the residuals table in `docs/ux-audit-2.md`.
#
# NOT in `make gates`: it is live, it is slow, and it is a report rather than a regression test.
# The regressions it found are pinned by `scripts/tui/16-*.sh` … `25-*.sh`, which ARE in the gates.
ux2: release ## the phase ux1 UX re-audit (live; needs ~/.bough/env with ANTHROPIC_API_KEY)
	@test -f $(HOME)/.bough/env || test -n "$$ANTHROPIC_API_KEY" || \
	  { echo "make ux2: no ANTHROPIC_API_KEY and no $(HOME)/.bough/env"; exit 1; }
	@BOUGH_BIN=$(CURDIR)/target/release/bough ./scripts/ux2/run.sh
