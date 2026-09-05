package main

// Where `bough update` gets main when there is no checkout to pull.
//
// A binary install used to be told to re-run the installer or `brew
// upgrade`, both of which fetch the newest TAG. Tags lag main by
// whatever has landed since the last release, so `bough update` did not
// mean "update" so much as "go back to the last release". It now builds
// the newest commit on main, which is what the command says it does.
//
// The clone is bough's, under ~/.bough/src, and is only ever
// fast-forwarded to origin/main — it is a cache, not somewhere to work.
// A checkout of your own still wins (see findCheckout): that is your
// tree and your branch, and update pulls it as it always did.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

// defaultUpstream is the repository a source-less install builds from.
const defaultUpstream = "https://github.com/andreylukin/bough"

// upstreamURL is where to clone from. $BOUGH_UPSTREAM overrides it —
// for a fork, a mirror inside a network that cannot reach GitHub, and
// for the tests, which must not clone the internet to check that
// cloning works.
func upstreamURL() string {
	if u := os.Getenv("BOUGH_UPSTREAM"); u != "" {
		return u
	}
	return defaultUpstream
}

// sourceDir is the managed clone's path.
func sourceDir(home string) string { return filepath.Join(home, ".bough", "src") }

// fetchMain returns a checkout of the newest commit on main, cloning it
// on first use and fast-forwarding it after that. The directory is
// bough's own, so resetting to origin/main is safe: nothing of yours
// lives there.
func fetchMain(home string) (string, error) {
	if err := haveTools(); err != nil {
		return "", err
	}
	dir := sourceDir(home)
	if _, err := os.Stat(filepath.Join(dir, ".git")); err != nil {
		if err := os.MkdirAll(filepath.Dir(dir), 0o755); err != nil {
			return "", fmt.Errorf("update: %w", err)
		}
		fmt.Printf("bough: no checkout found; cloning %s into %s\n", upstreamURL(), tildePath(home, dir))
		step("clone", filepath.Dir(dir), "git", "clone", "--depth", "1", "--branch", "main", upstreamURL(), dir)
		return dir, nil
	}
	step("fetch", dir, "git", "fetch", "--depth", "1", "origin", "main")
	// Hard, not merge: this clone exists to mirror main, and a local
	// edit in it would otherwise wedge every future update.
	step("checkout", dir, "git", "reset", "--hard", "FETCH_HEAD")
	return dir, nil
}

// haveTools reports whether the machine can build from source at all.
// Without them the honest answer is the release, so say so rather than
// failing halfway through a clone.
func haveTools() error {
	var missing []string
	for _, bin := range []string{"git", "go"} {
		if _, err := exec.LookPath(bin); err != nil {
			missing = append(missing, bin)
		}
	}
	if len(missing) == 0 {
		return nil
	}
	return fmt.Errorf(`bough update builds the newest commit on main, which needs %v on PATH.

Without a toolchain, install the latest RELEASE instead — it lags main
by whatever has landed since the last tag:
    curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh | sh
    brew upgrade bough`, missing)
}

// tildePath shortens a path under home for display.
func tildePath(home, p string) string {
	if rel, err := filepath.Rel(home, p); err == nil && !filepath.IsAbs(rel) && rel[0] != '.' {
		return "~/" + rel
	}
	return p
}
