`sse/*.sse` are Messages API streams in the shapes the claude-api skill
documents (thinking with signature deltas, redacted thinking, tool_use
input_json_delta, refusal stop_details, the server-side fallback block,
`display: "updates"` progress blocks, an in-stream error). They were
written by hand, not recorded: the ANTHROPIC_API_KEY available when W1
ran was rejected with a 401. Re-record them from a live key when one is
at hand; the tests read the files and nothing else.

`render/*.json` and `decode/*.json` are goldens: regenerate with
`go test ./internal/messagesapi -run 'Golden' -update`, never by hand.
