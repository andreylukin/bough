// Package messagesapi is the engine's Anthropic adapter: a harness
// llm.Adapter over the Messages API (beta namespace, always streaming)
// that renders the harness's flat item list into alternating messages
// and decodes the accumulated stream back.
//
// The render is a pure function of the items (Render) and every lossy
// decision is made once, at decode (Decode). That split is what keeps
// the transcript append-only across requests: a tool call that outlives
// its request gets a "still running" placeholder as its tool_result, and
// the real result arrives later as a <tool_result call_id=… name=…> text
// block, never as an edit of what was already sent. An edit would
// rewrite the prompt cache and fail the preserved-thinking check on
// Opus 5.5 and Fable 5.1. TestRenderIsAppendOnly is the gate for it.
//
// Retries all live here (errors.go): rate limits wait as told, overloads
// (including an overloaded_error inside a 200 stream) back off harder,
// cut streams and 5xx retry briefly, a refused beta is dropped for the
// process, and a thinking-binding 400 strips thinking for the adapter.
// An error that escapes is a turn-level failure in the engine's Gate.
//
// # Probes (go/docs/unreal-engine.md §15.3), run 2026-09-22
//
// The ANTHROPIC_API_KEY available for this run was rejected (HTTP 401),
// so every probe against the Messages API is unrun:
//
//	P1 Opus 5.5 placeholder then late result, block_binding error: skipped (key rejected)
//	P2 P1 with cache-diagnosis-2026-04-07:                         skipped (key rejected)
//	P3 longest SSE gap at max effort (sizes IdleTimeout, 5m now):  skipped (key rejected)
//	P4 display "updates" on Opus 5.5 and Fable 5.1:                skipped (key rejected)
//	P7 retired tool in history, Messages API:                      skipped (key rejected)
//
// The probe code is in probe_test.go; run it with a working key:
//
//	BOUGH_LIVE=1 BOUGH_PROBES=1 go test -run TestProbe -v ./internal/messagesapi/
//
// The Responses-side probes ran (internal/unreal/responses/probe_test.go):
//
//	P5 OpenRouter + anthropic/claude-sonnet-5, a second function_call_output
//	   for one call id: HTTP 200, and the model answered as if only the
//	   placeholder existed. OpenRouter drops the second output silently,
//	   so late_results must stay "text" for anthropic/* (the auto default);
//	   "native" loses every late result without an error.
//	P6 OpenRouter + anthropic/claude-sonnet-5, reasoning (reasoning_text
//	   plus signature) sent back through Responses: accepted untouched;
//	   with the signature tampered, HTTP 400 "Invalid `signature` in
//	   `thinking` block" from the upstream. The signature is forwarded and
//	   checked, so reasoning replay is sound for that family, and
//	   StripReasoningOn400 recognises the failure.
//	P7 a historical call to a tool no longer in tools: accepted by
//	   OpenRouter anthropic/claude-sonnet-5, OpenRouter openai/gpt-5.6-sol
//	   and OpenAI gpt-5.6-sol. Unprobed on the Messages API (above).
package messagesapi
