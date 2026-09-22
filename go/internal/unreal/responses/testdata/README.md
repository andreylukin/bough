`*.sse` and `*.req.json` were recorded from the real APIs on
2026-09-22 by `record_test.go` (the contract tape: ask, call
`read_secret`, answer with its result; and one reply that reasons):

    BOUGH_LIVE=1 go test -tags record -run TestRecord ./internal/unreal/responses/

Only bodies are written; no header, so no key, reaches a file. The
`*.deltas` goldens are what the tap streams from each recording:
regenerate them with `go test ./internal/unreal/responses -run TestTapGoldens -update`.
