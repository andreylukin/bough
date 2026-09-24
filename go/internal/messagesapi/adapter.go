package messagesapi

import (
	"context"
	stdjson "encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"slices"
	"sync"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	"github.com/anthropics/anthropic-sdk-go/shared/constant"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

// Config is one llm-anthropic row's adapter for one session.
type Config struct {
	Client      anthropic.Client // option.WithMaxRetries(0); bough's bounded *http.Client; no overall timeout
	Model       func() string    // the llm row's live model
	Effort      func() string    // the llm row's live bough effort level
	MaxTokens   int64            // 0 = 64000, capped per ModelSpec
	CacheTTL    string           // "1h" (default) | "5m"
	Display     string           // "" = per-model table (§7.2)
	Fallbacks   string           // "auto" (default) | "off"
	Binding     string           // "" = none sent | "drop_block" | "error" (probes and tests)
	MaxAttempts int              // 0 = the §7.2 schedule
	IdleTimeout time.Duration    // 0 = 5m until probe P3 sizes it
	Options     agentllm.Options
	// Wait is the pause before a retry; nil sleeps. llm-control holds it
	// until the test lets the retry go, so a model test can stand in the
	// wait (and stop the turn there) instead of racing a timer.
	Wait func(ctx context.Context, d time.Duration) error
}

// defaultIdleTimeout bounds silence inside a started stream. The SDK
// drops ping events before bough sees them, so the watchdog reads the
// body itself: every byte, pings included, resets it. Five minutes
// matches the loop's stall guard until probe P3 measures the longest
// real gap under max-effort thinking.
const defaultIdleTimeout = 5 * time.Minute

type adapter struct {
	c Config

	// stale is the thinking blocks a binding 400 condemned, by their
	// raw bytes. Only those are stripped from later requests: a block
	// the model produces after the strip was bound against the
	// stripped history, stays valid, and is the interleaved reasoning
	// the rest of the session would otherwise lose (the API calls
	// strip-and-retry a one-time recovery, not a steady state).
	mu    sync.Mutex
	stale map[string]bool

	// wait and now are the adapter's only clock; tests replace them so
	// a retry schedule is checked without sleeping through it.
	wait func(ctx context.Context, d time.Duration) error
	now  func() time.Time
	// dropped is processDropped; a test gets its own so a refused beta
	// does not leak into the tests running beside it.
	dropped *sync.Map
}

// New validates the config and returns the adapter. Provider() is
// "anthropic".
func New(c Config) (agentllm.Adapter, error) {
	if c.Model == nil {
		return nil, errors.New("llm-anthropic: the adapter needs the row's model")
	}
	if c.Effort == nil {
		c.Effort = func() string { return "" }
	}
	switch c.CacheTTL {
	case "":
		c.CacheTTL = "1h"
	case "1h", "5m":
	default:
		return nil, fmt.Errorf("llm-anthropic: cache_ttl must be 1h or 5m, got %q", c.CacheTTL)
	}
	switch c.Display {
	case "auto":
		c.Display = ""
	case "", "summarized", "omitted", "updates":
	default:
		return nil, fmt.Errorf("llm-anthropic: thinking_display must be auto, summarized, omitted or updates, got %q", c.Display)
	}
	switch c.Fallbacks {
	case "":
		c.Fallbacks = "auto"
	case "auto", "off":
	default:
		return nil, fmt.Errorf("llm-anthropic: fallbacks must be auto or off, got %q", c.Fallbacks)
	}
	switch c.Binding {
	case "", "drop_block", "error":
	default:
		return nil, fmt.Errorf("llm-anthropic: block_binding must be drop_block or error, got %q", c.Binding)
	}
	if c.MaxAttempts < 0 {
		return nil, fmt.Errorf("llm-anthropic: max_attempts must be 0 or more, got %d", c.MaxAttempts)
	}
	if c.IdleTimeout <= 0 {
		c.IdleTimeout = defaultIdleTimeout
	}
	wait := sleep
	if c.Wait != nil {
		wait = c.Wait
	}
	return &adapter{c: c, wait: wait, now: time.Now, dropped: &processDropped}, nil
}

func (a *adapter) Provider() string { return "anthropic" }
func (a *adapter) Model() string    { return a.c.Model() }
func (a *adapter) Close() error     { return nil }

func sleep(ctx context.Context, d time.Duration) error {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-t.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// withoutStale drops the thinking blocks an earlier binding 400
// condemned and keeps every other item.
func (a *adapter) withoutStale(in []ullm.Item) []ullm.Item {
	a.mu.Lock()
	defer a.mu.Unlock()
	if len(a.stale) == 0 {
		return in
	}
	out := make([]ullm.Item, 0, len(in))
	for _, it := range in {
		if d, ok := it.Data.(ullm.Reasoning); ok && it.Type == ullm.ItemReasoning && a.stale[string(d.Raw)] {
			continue
		}
		out = append(out, it)
	}
	return out
}

// condemn marks every thinking block in a request the API refused as
// bound to another conversation. The error names only the first bad
// block, and the API drops that one and every later one anyway, so
// all of the request's blocks go.
func (a *adapter) condemn(in []ullm.Item) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.stale == nil {
		a.stale = map[string]bool{}
	}
	for _, it := range in {
		if d, ok := it.Data.(ullm.Reasoning); ok && it.Type == ullm.ItemReasoning {
			a.stale[string(d.Raw)] = true
		}
	}
}

// Respond renders, streams and decodes, retrying what is worth retrying.
// Every retry lives here: an error this returns ends the request in the
// Gate, so giving up is a turn-level failure.
func (a *adapter) Respond(ctx context.Context, r ullm.Request, _ ullm.RequestOptions) (ullm.Response, error) {
	model := a.c.Model()
	spec := Spec(model)
	if a.c.Display != "" {
		spec.Display = a.c.Display
	}
	req := r
	req.Model.ID = model
	req.Model.ReasoningEffort = EffortFor(a.c.Effort(), spec)
	maxTokens := a.c.MaxTokens
	if maxTokens <= 0 {
		maxTokens = defaultMaxTokens
	}
	req.Model.MaxOutputTokens = &maxTokens

	seq := agentllm.SeqOf(ctx)
	retriedBeta, retriedBinding := false, false
	for attempt := 1; ; attempt++ {
		if spec.Display == "updates" && a.betaDropped(anthropic.AnthropicBetaThinkingDisplayUpdates2026_08_18) {
			spec.Display = "summarized"
		}
		req.Input = a.withoutStale(req.Input)
		params, err := Render(req, spec, a.c.CacheTTL)
		if err != nil {
			return ullm.Response{}, fmt.Errorf("llm-anthropic: %w", err)
		}
		a.extras(&params, spec)

		msg, err := a.attempt(ctx, params, seq, attempt)
		if err == nil {
			resp, derr := Decode(msg)
			if derr == nil {
				return resp, nil
			}
			if !errors.Is(derr, errCut) {
				return ullm.Response{}, fmt.Errorf("llm-anthropic: %w", derr)
			}
			err = derr
		}
		if ctx.Err() != nil {
			return ullm.Response{}, ctx.Err()
		}
		f := classify(err, a.now())
		switch f.class {
		case badBeta:
			if retriedBeta || len(f.betas) == 0 {
				return ullm.Response{}, f.final(model)
			}
			retriedBeta = true
			for _, b := range f.betas {
				a.dropped.Store(b, true)
			}
			continue
		case binding:
			if retriedBinding {
				return ullm.Response{}, f.final(model)
			}
			retriedBinding = true
			a.condemn(req.Input)
			continue
		case overflow, fatal:
			return ullm.Response{}, f.final(model)
		}
		attempts, delays := f.budget(a.c.MaxAttempts)
		if attempt >= attempts {
			return ullm.Response{}, f.final(model)
		}
		d := f.after
		if d <= 0 {
			d = delays[min(attempt-1, len(delays)-1)]
		}
		if sink := a.c.Options.Sink; sink != nil {
			why := f.msg
			if why == "" {
				why = err.Error()
			}
			sink(agentllm.Delta{Seq: seq, Attempt: attempt + 1, Kind: agentllm.DeltaRetry, Wait: d, Err: why})
		}
		if err := a.wait(ctx, d); err != nil {
			return ullm.Response{}, err
		}
	}
}

// extras adds what Render leaves to the adapter because it depends on
// the process, not the request: betas the API refused, fallbacks, and
// the binding control.
func (a *adapter) extras(p *anthropic.BetaMessageNewParams, spec ModelSpec) {
	fb := anthropic.AnthropicBetaServerSideFallback2026_07_01
	if a.c.Fallbacks == "auto" && slices.Contains(fallbackFamilies, spec.Family) && !a.betaDropped(fb) {
		p.Fallbacks = anthropic.BetaFallbacksParamUnion{OfDefault: constant.ValueOf[constant.Default]()}
		p.Betas = append(p.Betas, fb)
	}
	bb := anthropic.AnthropicBetaThinkingBindingControls2026_08_01
	if a.c.Binding != "" && p.Thinking.OfAdaptive != nil && !a.betaDropped(bb) {
		ad := *p.Thinking.OfAdaptive
		ad.BlockBinding = anthropic.BetaThinkingBlockBindingParam{PrefixMismatchBehavior: anthropic.BetaThinkingPrefixMismatchBehavior(a.c.Binding)}
		p.Thinking.OfAdaptive = &ad
		p.Betas = append(p.Betas, bb)
	}
	var betas []anthropic.AnthropicBeta
	for _, b := range p.Betas {
		if !a.betaDropped(b) && !slices.Contains(betas, b) {
			betas = append(betas, b)
		}
	}
	p.Betas = betas
}

// attempt is one streamed request.
func (a *adapter) attempt(ctx context.Context, params anthropic.BetaMessageNewParams, seq uint64, attempt int) (anthropic.BetaMessage, error) {
	w := &watch{limit: a.c.IdleTimeout}
	stream := a.c.Client.Beta.Messages.NewStreaming(ctx, params, option.WithMiddleware(w.middleware))
	defer stream.Close()
	sink := a.c.Options.Sink
	emit := func(d agentllm.Delta) {
		if sink != nil {
			d.Seq, d.Attempt = seq, attempt
			sink(d)
		}
	}
	var msg anthropic.BetaMessage
	complete := false
	var accErr error
	for stream.Next() {
		ev := stream.Current()
		if err := msg.Accumulate(ev); err != nil {
			accErr = fmt.Errorf("messagesapi: %w", err)
			break
		}
		switch ev.Type {
		case "content_block_start":
			if ev.ContentBlock.Type == "tool_use" {
				emit(agentllm.Delta{Kind: agentllm.DeltaToolStart, CallID: ev.ContentBlock.ID, Name: ev.ContentBlock.Name})
			}
		case "content_block_delta":
			switch ev.Delta.Type {
			case "text_delta":
				if ev.Delta.Text != "" {
					emit(agentllm.Delta{Kind: agentllm.DeltaText, Text: ev.Delta.Text})
				}
			case "thinking_delta":
				if ev.Delta.Thinking != "" {
					emit(agentllm.Delta{Kind: agentllm.DeltaThinking, Text: ev.Delta.Thinking})
				}
			}
		case "message_stop":
			complete = true
		}
	}
	err := accErr
	if err == nil {
		err = stream.Err()
	}
	if err != nil && w.tripped() {
		err = fmt.Errorf("%w for %s: %w", errIdle, a.c.IdleTimeout, errCut)
	}
	if err == nil && !complete {
		err = errCut
	}
	a.trace(params, attempt, w.status(), msg, err)
	return msg, err
}

func (a *adapter) trace(params anthropic.BetaMessageNewParams, attempt, status int, msg anthropic.BetaMessage, err error) {
	fn := a.c.Options.Trace
	if fn == nil {
		return
	}
	ex := agentllm.Exchange{Provider: "anthropic", Attempt: attempt, Status: status}
	if b, e := stdjson.Marshal(params); e == nil {
		ex.Request = b
	}
	var api *anthropic.Error
	switch {
	case errors.As(err, &api):
		ex.Response = []byte(api.RawJSON())
	case msg.RawJSON() != "":
		ex.Response = []byte(msg.RawJSON())
	}
	if err != nil {
		ex.Err = err.Error()
	}
	fn(ex)
}

// watch is the idle watchdog on one response body.
type watch struct {
	limit time.Duration

	mu     sync.Mutex
	code   int
	trip   bool
	closed bool
	timer  *time.Timer
	body   io.ReadCloser
}

func (w *watch) middleware(req *http.Request, next option.MiddlewareNext) (*http.Response, error) {
	resp, err := next(req)
	if err != nil || resp == nil {
		return resp, err
	}
	w.mu.Lock()
	w.code = resp.StatusCode
	w.mu.Unlock()
	if resp.StatusCode == http.StatusOK && resp.Body != nil {
		w.body = resp.Body
		w.timer = time.AfterFunc(w.limit, w.fire)
		resp.Body = w
	}
	return resp, nil
}

func (w *watch) fire() {
	w.mu.Lock()
	defer w.mu.Unlock()
	if !w.closed {
		w.trip = true
		w.closed = true
		w.body.Close()
	}
}

func (w *watch) Read(p []byte) (int, error) {
	n, err := w.body.Read(p)
	if n > 0 {
		w.mu.Lock()
		if !w.closed {
			w.timer.Reset(w.limit)
		}
		w.mu.Unlock()
	}
	return n, err
}

func (w *watch) Close() error {
	w.mu.Lock()
	first := !w.closed
	w.closed = true
	w.mu.Unlock()
	w.timer.Stop()
	if first {
		return w.body.Close()
	}
	return nil
}

func (w *watch) tripped() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.trip
}

func (w *watch) status() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.code
}
