#!/usr/bin/env bash
# Fresh-machine bootstrap for bough. macOS and Linux — nothing here is confined;
# bough runs as you (spec §2). Installs toolchain deps with the platform's package
# manager, installs the npm dependencies, and links the `bough` server manager onto
# PATH. Safe to re-run.
#
# WHAT IS AND IS NOT PLATFORM-SPECIFIC. The dependency LIST is identical everywhere
# (git, node, rg, uv, bun); only the command that installs it differs. So this file
# resolves one installer up front and the rest reads the same on both — which is
# also why a distro nobody here has tested is a missing installer line and not a
# port.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

OS="$(uname -s)"

# One installer, resolved once. `install_pkgs` takes the PACKAGE names for this
# platform; every caller below passes the same logical set.
if [ "$OS" = "Darwin" ]; then
  if ! command -v brew >/dev/null; then
    echo "error: Homebrew is required. Install it first:" >&2
    echo '  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"' >&2
    exit 1
  fi
  install_pkgs() { brew install "$@"; }
  PKG_RG=ripgrep PKG_NODE=node PKG_UV=uv
elif command -v apt-get >/dev/null; then
  install_pkgs() { sudo apt-get install -y "$@"; }
  PKG_RG=ripgrep PKG_NODE=nodejs PKG_UV=
elif command -v dnf >/dev/null; then
  install_pkgs() { sudo dnf install -y "$@"; }
  PKG_RG=ripgrep PKG_NODE=nodejs PKG_UV=
elif command -v pacman >/dev/null; then
  install_pkgs() { sudo pacman -S --needed --noconfirm "$@"; }
  PKG_RG=ripgrep PKG_NODE=nodejs PKG_UV=uv
else
  # Named rather than guessed at: a wrong `sudo <installer> install` is worse than
  # a sentence telling you what to install.
  echo "error: no supported package manager found on $OS (looked for brew, apt-get, dnf, pacman)." >&2
  echo "  install these yourself, then re-run: git, node, ripgrep, uv" >&2
  exit 1
fi

if ! command -v git >/dev/null; then
  if [ "$OS" = "Darwin" ]; then
    echo "error: git not found — install the Xcode Command Line Tools: xcode-select --install" >&2
  else
    echo "error: git not found — install it with your package manager, then re-run." >&2
  fi
  exit 1
fi

# rg is what the prompt tells the model to search with. (bun has its own block
# below — it needs a version floor, and may already be on PATH from the bun.sh
# installer.) There is no local inference: the cheap tier is a hosted model you pick
# in the model picker, so no llama.cpp and no GGUF. There is no tunnel: the server
# binds loopback and has no auth layer (spec §17).
echo "==> checking packages"
need_bins=(node rg uv)
need_pkgs=("$PKG_NODE" "$PKG_RG" "$PKG_UV")
missing=()
for i in "${!need_bins[@]}"; do
  command -v "${need_bins[$i]}" >/dev/null && continue
  # An empty package name means this platform has no distro package for it —
  # `uv` on Debian/Fedora — so it is reported instead of silently skipped.
  if [ -z "${need_pkgs[$i]}" ]; then
    echo "note: ${need_bins[$i]} has no package here — install it with:" >&2
    echo "  curl -LsSf https://astral.sh/uv/install.sh | sh" >&2
  else
    missing+=("${need_pkgs[$i]}")
  fi
done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "==> installing ${missing[*]}"
  install_pkgs "${missing[@]}"
else
  echo "==> all packages already installed"
fi

# Bun >= 1.3. That is the floor the tree is developed and tested against. On macOS
# we install/upgrade through brew even when an older bun is already on PATH from
# the bun.sh installer (brew upgrade would fail on that one); elsewhere the bun.sh
# installer IS the supported path, and it upgrades in place.
# ~/.bun/bin is where the bun.sh installer puts it, and it is on PATH only if the
# user's shell rc adds it. Looked at BEFORE deciding bun is missing, so a perfectly
# good bun does not get a redundant brew install stacked on top of it.
PATH_BEFORE_BUN="$PATH" # kept so the advisory below can tell "found it" from "you had it"
case ":$PATH:" in
  *":$HOME/.bun/bin:"*) ;;
  *) [ -d "$HOME/.bun/bin" ] && PATH="$PATH:$HOME/.bun/bin" && export PATH ;;
esac

bun_ok() {
  command -v bun >/dev/null || return 1
  local v
  v="$(bun --version | head -1)"
  [ "$(printf '%s\n' "1.3.0" "$v" | sort -V | head -1)" = "1.3.0" ]
}
if bun_ok; then
  echo "==> bun $(bun --version) ok"
elif [ "$OS" = "Darwin" ] && brew list --formula oven-sh/bun/bun >/dev/null 2>&1; then
  echo "==> upgrading bun via brew"
  brew upgrade oven-sh/bun/bun
elif [ "$OS" = "Darwin" ]; then
  echo "==> installing bun via brew"
  brew install oven-sh/bun/bun
else
  echo "==> installing bun via bun.sh"
  curl -fsSL https://bun.sh/install | bash
  case ":$PATH:" in
    *":$HOME/.bun/bin:"*) ;;
    *) [ -d "$HOME/.bun/bin" ] && PATH="$PATH:$HOME/.bun/bin" && export PATH ;;
  esac
fi
if ! bun_ok; then
  echo "error: need bun >= 1.3 on PATH — have '$(command -v bun || echo none)'." >&2
  if [ "$OS" = "Darwin" ]; then
    echo "  A non-brew bun (e.g. ~/.bun/bin) may be shadowing brew's; fix your PATH so" >&2
    echo "  $(brew --prefix)/bin comes first, or remove the old bun." >&2
  else
    echo "  An older bun may be shadowing the one just installed; make sure" >&2
    echo "  ~/.bun/bin comes first on PATH, or remove the old bun." >&2
  fi
  exit 1
fi

# bun works for US because of the PATH line above, but the user's own shell was
# never told. `bough` self-heals the same way, so the server and the TUI are fine —
# what breaks is every `bun test` / `bun run check` typed by hand, which fails with
# "command not found" on a machine where bun is plainly installed. Say the fix once,
# with the actual file and the actual line, rather than leaving it to be rediscovered.
case "$(command -v bun)" in
  "$HOME/.bun/bin/"*)
    case ":${PATH_BEFORE_BUN:-$PATH}:" in
      *":$HOME/.bun/bin:"*) ;;
      *)
        rc="$HOME/.zshrc"
        case "${SHELL:-}" in *bash) rc="$HOME/.bash_profile" ;; esac
        echo "==> note: bun is in ~/.bun/bin, which is not on your shell's PATH."
        echo "    bough itself does not care — it looks there. Your own shell does:"
        echo "        echo 'export PATH=\"\$HOME/.bun/bin:\$PATH\"' >> $rc"
        echo "    then open a new shell (or: source $rc)."
        ;;
    esac
    ;;
esac

# leta: LSP backend for the lsp.* host functions (symbol navigation).
# From a third-party tap, so it can't go in the main package array above — and the
# tap is macOS-only. OPTIONAL EVERYWHERE: without it `lsp.*` is simply not granted
# and the prompt tells the model to use rg + view instead (`prompt/searching.md`),
# so a machine that cannot install it gets one fewer verb, not a broken bough.
if command -v leta >/dev/null; then
  echo "==> leta already installed"
elif [ "$OS" = "Darwin" ]; then
  echo "==> installing leta (LSP backend) via brew tap"
  brew install andreasjansson/tap/leta
else
  echo "==> leta not installed (no tap on $OS) — lsp.* will fall back to rg + view"
  echo "    to add it: https://github.com/andreasjansson/leta"
fi

# typescript-language-server + typescript@5: leta's tsserver for TS/JS navigation.
# TS7 ships no tsserver.js, so pin typescript@5.
if ! command -v typescript-language-server >/dev/null; then
  echo "==> installing typescript-language-server + typescript@5 (npm global)"
  npm install -g typescript-language-server typescript@5
else
  echo "==> typescript-language-server already installed"
fi

echo "==> installing dependencies + typecheck"
(cd "$ROOT" && bun install && bun run check)


echo "==> linking bough CLI to ~/.local/bin/bough"
mkdir -p "$HOME/.local/bin"
ln -sf "$ROOT/scripts/bough" "$HOME/.local/bin/bough"
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) echo "warning: ~/.local/bin is not in PATH — add it to your shell profile:" >&2
     echo '  export PATH="$HOME/.local/bin:$PATH"' >&2 ;;
esac

# Env file the `bough` manager (and its launchd service) sources on start.
# Never commit this.
ENV_FILE="$HOME/.bough/env"
if [ ! -f "$ENV_FILE" ]; then
  echo "==> writing env template to $ENV_FILE"
  mkdir -p "$HOME/.bough"
  cat > "$ENV_FILE" <<'EOF'
# Environment for the bough server, sourced by `bough start` / the launchd service.
ANTHROPIC_API_KEY=
# OPENAI_API_KEY=          # only for the openai: entries in the model picker
# OPENROUTER_API_KEY=      # only for the vendor/model entries in the model picker
# BOUGH_PORT=4321
# BOUGH_CHEAP_MODEL=       # titles, ghost text, activity blurbs. Default: a cheap
#                          # frontier model. Also settable in the model picker.
EOF
  chmod 600 "$ENV_FILE"
fi

echo
echo "setup complete. Next:"
echo "  1. put your ANTHROPIC_API_KEY in $ENV_FILE"
echo "  2. bough start"
echo "(or run 'bough setup' — it walks both steps interactively)"
