# bough — a coding agent that acts by writing programs.
#
# WHERE THIS FILE IS USED FROM. Here, by `brew tap andreylukin/bough <this
# repo's URL>`, and — once the tap repo exists — as the copy in
# `andreylukin/homebrew-bough`, which is what makes the short
# `brew install andreylukin/bough/bough` resolve. This file is the source of
# truth for both; a release tags the commit and bumps `url`/`version`/`sha256`
# here first. The url names a TAG, not a commit sha: a commit pin goes stale on
# the next push and quietly ships a `brew install` of yesterday's tree.
#
# WHAT THIS FORMULA INSTALLS. One static binary and nothing else. bough used to
# be a Rust server with a lifecycle script, a LaunchAgent, and a runtime set of
# node/ripgrep/ast-grep/uv; that tree is gone. The Go bough is a single
# terminal program with no sidecar, no service to manage, and no external
# runtime — code mode runs JavaScript in-process (goja) and SQLite is pure Go
# (modernc.org/sqlite), so there is nothing here to depend on but a build
# toolchain.
class Bough < Formula
  desc "Coding agent that acts by writing programs"
  homepage "https://github.com/andreylukin/bough"
  url "https://github.com/andreylukin/bough/archive/refs/tags/v0.2.3.tar.gz"
  sha256 "753c5a678444cbce393f03ec1add63a6ae1e53727d1ee24d269ccc6f73996d3f"
  license "Apache-2.0"
  head "https://github.com/andreylukin/bough.git", branch: "main"

  depends_on "go" => :build

  def install
    # The version the binary reports has to be the tag brew installed, not the
    # "dev" a checkout-less build would otherwise fall back to: `bough
    # --version` is the first thing a bug report quotes.
    ldflags = "-s -w -X main.version=v#{version}"
    system "go", "build", *std_go_args(ldflags: ldflags), "./cmd/bough"
  end

  def caveats
    <<~EOS
      Put an API key where bough reads it, then start it:

        mkdir -p ~/.bough
        echo 'ANTHROPIC_API_KEY=sk-ant-...' >> ~/.bough/env
        bough

      ~/.bough/env is read at boot, so keys never need to live in your shell.
      OPENAI_API_KEY, OPENROUTER_API_KEY and CEREBRAS_API_KEY work too; swap
      providers with /model or the llm row in ~/.bough/bough.yml.

      There is NO isolation boundary: programs run as you, with your full
      authority. Run bough only where you would run the code it writes.
    EOS
  end

  test do
    # The binary reports the version brew built, and the config tree mounts
    # without a network or an API key: --dump-config prints the row table and
    # exits, which fails loudly if any plugin's Apply is broken.
    assert_match version.to_s, shell_output("#{bin}/bough --version")
    assert_match "codemode", shell_output("#{bin}/bough --dump-config")
  end
end
