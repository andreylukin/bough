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
# WHAT THIS FORMULA INSTALLS. Two files and a wrapper, because bough is two
# things: the Rust binary (server, TUI, `exec`, the CLIs — one build) and the
# lifecycle script that owns `start`/`kill`/`restart`/`status`/`logs`, which are
# launchd and systemd verbs the binary has no equivalent for. The script is the
# `bough` a person types; it finds its binary through BOUGH_BIN.
#
# Every path in the wrapper goes through `opt_libexec`, never the Cellar: the
# LaunchAgent the script writes names its own path, and a version-pinned one
# would point at a directory `brew upgrade` has already deleted.
class Bough < Formula
  desc "Coding agent that acts by writing programs"
  homepage "https://github.com/andreylukin/bough"
  url "https://github.com/andreylukin/bough/archive/refs/tags/v0.1.0.tar.gz"
  version "0.1.0"
  sha256 "89ea82b6219f70ccd0934560d4e8f695525113c509fe117f4201cc9a0cd526ed"
  license "Apache-2.0"
  head "https://github.com/andreylukin/bough.git", branch: "main"

  depends_on "rust" => :build

  # The runtime set is the one `scripts/setup.sh` installs, and it is not
  # decoration: `node` runs the code-mode sidecar (bough uses `bun` instead when
  # it is on PATH — that is an upgrade, never a requirement), and the system
  # prompt names `rg` and `ast-grep` unconditionally, so an install without them
  # documents tools it does not have.
  depends_on "ast-grep"
  depends_on "node"
  depends_on "ripgrep"
  depends_on "uv"

  def install
    system "cargo", "build", "--release", "--locked", "--package", "bough"

    libexec.install "target/release/bough" => "bough-bin"
    libexec.install "scripts/bough"

    # The command on PATH. It names the binary and the installer so the script
    # never has to guess at either: `bough update` is git-and-cargo against a
    # source tree this layout does not have, and BOUGH_INSTALLER is what lets it
    # answer "brew upgrade bough" instead of failing in a package prefix.
    (bin/"bough").write <<~BASH
      #!/bin/bash
      export BOUGH_BIN="#{opt_libexec}/bough-bin"
      export BOUGH_INSTALLER="homebrew"
      exec /bin/bash "#{opt_libexec}/bough" "$@"
    BASH
  end

  def caveats
    <<~EOS
      Put an API key in ~/.bough/env, then start the server:

        bough setup     # prompts for ANTHROPIC_API_KEY and starts the service
        bough           # the TUI

      `bough start` installs bough's own LaunchAgent (systemd user unit on
      Linux) — this is deliberately not a `brew services` formula, because two
      service managers pointed at one server is a way to have neither work.

      There is NO isolation boundary: programs run as you, with your full
      authority. Run bough only where you would run the code it writes.

      Update with `brew upgrade bough`. (`bough update` is for the from-source
      install, which has a checkout to pull; this one does not.)
    EOS
  end

  test do
    # Exercises the whole chain the wrapper sets up — shim, script, binary —
    # and needs no server, no key and no network.
    assert_match "bough #{version}", shell_output("#{bin}/bough --version")

    # `update` in a package prefix must refuse and name the command that works,
    # rather than running git against a directory that has no repository in it.
    output = shell_output("#{bin}/bough update 2>&1", 1)
    assert_match "brew upgrade bough", output
  end
end
