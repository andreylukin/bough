# Screenshots

Everything here is recorded by [scripts/demo](scripts/demo) against the same small repo with two failing tests, using a real model. Re-run the scripts to refresh them.

## Terminal UI

One turn: bough runs the failing tests, reads the code, patches `TopN`, and runs the tests green.

<p align="center"><img src="assets/demo.gif" width="820" alt="bough fixing two failing Go tests in one turn"></p>

## Web control room (`bough serve`)

A screenshot pasted into the composer goes to the model as an image.

<p align="center"><img src="assets/web-image.png" width="820" alt="a session started from a pasted image"></p>
