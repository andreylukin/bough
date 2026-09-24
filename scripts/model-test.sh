#!/usr/bin/env bash
# model-test: runs the FizzBee models' offline tests — the fetch script's
# own checks, the Go trace checker and control model, and the TS path
# generator. All of them read the run dirs checked in under
# go/tests/model/testdata, so nothing here downloads fizz or the network.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

"$root/scripts/fizz_test.sh"
(cd "$root/go" && go test -race -parallel 4 ./tests/model/...)
# node strips the TS types itself (22.18+); the generator has no deps.
(cd "$root/go/tests/web" && npm run --silent test:model)
