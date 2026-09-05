package llm

// The HTTP client every provider shares, and the stall guard on a
// streaming body.
//
// All three HTTP providers used http.DefaultClient, which has no
// timeout of any kind. A connection that opens and then goes quiet —
// a server that never answers, a stream that dies mid-flight — hangs
// the turn forever. Watched it happen: a real run sat for eight
// minutes with no output and no error, and would have sat there until
// the process was killed.
//
// A plain Client.Timeout is the wrong instrument, because it bounds the
// WHOLE request including the body, and a long reply legitimately
// streams for minutes. What can be bounded is silence: how long to
// wait for the response to start, and how long a started stream may go
// without producing a byte.

import (
	"io"
	"net"
	"net/http"
	"sync"
	"time"
)

const (
	// dialTimeout bounds getting a connection at all.
	dialTimeout = 30 * time.Second
	// headerTimeout bounds the wait for response headers after the
	// request is sent. Generous on purpose, and more generous than it
	// first looks: a STREAMING call sees headers almost at once, but a
	// non-streaming one (the small-model jobs, and anthropic's
	// non-stream path) may get nothing until the whole reply has been
	// generated — which for a reasoning model on a long answer is
	// minutes. A false timeout kills real work; a true one only delays
	// noticing a dead connection.
	headerTimeout = 5 * time.Minute
	// stallTimeout bounds silence WITHIN a started stream. Most
	// providers send keep-alive comments while a model thinks, and
	// every byte resets this — but a provider that sends none while a
	// reasoning model works for a long time would look identical to a
	// dead connection, so this matches headerTimeout rather than
	// guessing tighter. Noticing a dead stream in five minutes instead
	// of two is still the difference between recovering and hanging
	// forever.
	stallTimeout = 5 * time.Minute
)

// httpClient is shared by the HTTP providers. No Client.Timeout: see
// the note above.
var httpClient = &http.Client{
	Transport: &http.Transport{
		Proxy:                 http.ProxyFromEnvironment,
		DialContext:           (&net.Dialer{Timeout: dialTimeout, KeepAlive: 30 * time.Second}).DialContext,
		ForceAttemptHTTP2:     true,
		MaxIdleConns:          32,
		IdleConnTimeout:       90 * time.Second,
		TLSHandshakeTimeout:   dialTimeout,
		ExpectContinueTimeout: time.Second,
		ResponseHeaderTimeout: headerTimeout,
	},
}

// HTTPClient is the shared client, for a provider outside this package
// (or an SDK that takes one).
func HTTPClient() *http.Client { return httpClient }

// stallGuard wraps a streaming body so a read that produces nothing for
// stallTimeout fails instead of blocking forever. The timer is reset by
// every successful read, so a slow but live stream is never cut.
type stallGuard struct {
	body  io.ReadCloser
	limit time.Duration

	mu     sync.Mutex
	timer  *time.Timer
	closed bool
}

// guardStalls returns body wrapped so that silence longer than limit
// closes it, which surfaces to the reader as a failed read.
func guardStalls(body io.ReadCloser, limit time.Duration) io.ReadCloser {
	g := &stallGuard{body: body, limit: limit}
	g.timer = time.AfterFunc(limit, g.trip)
	return g
}

// trip closes the body, which unblocks whoever is reading it.
func (g *stallGuard) trip() {
	g.mu.Lock()
	defer g.mu.Unlock()
	if !g.closed {
		g.closed = true
		g.body.Close()
	}
}

func (g *stallGuard) Read(p []byte) (int, error) {
	n, err := g.body.Read(p)
	if n > 0 {
		g.mu.Lock()
		if !g.closed {
			g.timer.Reset(g.limit)
		}
		g.mu.Unlock()
	}
	return n, err
}

func (g *stallGuard) Close() error {
	g.mu.Lock()
	first := !g.closed
	g.closed = true
	g.mu.Unlock()
	g.timer.Stop()
	if first {
		return g.body.Close()
	}
	return nil
}
