package lsp

// A minimal JSON-RPC 2.0 client over a language server's stdio:
// Content-Length framing, request/response by id, and callbacks for the
// server's notifications and requests.

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func (e *rpcError) Error() string { return fmt.Sprintf("%s (code %d)", e.Message, e.Code) }

type message struct {
	JSONRPC string         `json:"jsonrpc"`
	ID      jsontext.Value `json:"id,omitempty"`
	Method  string         `json:"method,omitempty"`
	Params  jsontext.Value `json:"params,omitempty"`
	Result  jsontext.Value `json:"result,omitempty"`
	Error   *rpcError      `json:"error,omitempty"`
}

type conn struct {
	cmd    *exec.Cmd
	in     io.WriteCloser
	stderr *tailBuffer

	wmu  sync.Mutex
	next atomic.Int64

	pmu     sync.Mutex
	pending map[int64]chan message

	onNotify  func(method string, params jsontext.Value)
	onRequest func(method string, params jsontext.Value) any

	done chan struct{} // closed when the server's stdout ends
	err  error         // why it ended; read after done
}

// dial starts argv in dir and reads its stdout until it exits.
func dial(dir string, argv []string, onNotify func(string, jsontext.Value), onRequest func(string, jsontext.Value) any) (*conn, error) {
	cmd := exec.Command(argv[0], argv[1:]...)
	cmd.Dir = dir
	in, err := cmd.StdinPipe()
	if err != nil {
		return nil, err
	}
	out, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}
	c := &conn{cmd: cmd, in: in, stderr: &tailBuffer{max: 2048}, pending: map[int64]chan message{},
		onNotify: onNotify, onRequest: onRequest, done: make(chan struct{})}
	cmd.Stderr = c.stderr
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	go c.read(bufio.NewReader(out))
	return c, nil
}

func (c *conn) read(r *bufio.Reader) {
	defer func() {
		c.pmu.Lock()
		for id, ch := range c.pending {
			close(ch)
			delete(c.pending, id)
		}
		c.pmu.Unlock()
		close(c.done)
	}()
	for {
		body, err := readFrame(r)
		if err != nil {
			c.err = err
			return
		}
		var m message
		if err := json.Unmarshal(body, &m); err != nil {
			continue
		}
		switch {
		case m.Method != "" && len(m.ID) > 0:
			res := c.onRequest(m.Method, m.Params)
			raw, _ := json.Marshal(res)
			c.write(message{JSONRPC: "2.0", ID: m.ID, Result: raw})
		case m.Method != "":
			c.onNotify(m.Method, m.Params)
		default:
			id, err := strconv.ParseInt(string(m.ID), 10, 64)
			if err != nil {
				continue
			}
			c.pmu.Lock()
			ch := c.pending[id]
			delete(c.pending, id)
			c.pmu.Unlock()
			if ch != nil {
				ch <- m
			}
		}
	}
}

func readFrame(r *bufio.Reader) ([]byte, error) {
	length := -1
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			return nil, err
		}
		line = strings.TrimRight(line, "\r\n")
		if line == "" {
			break
		}
		if v, ok := strings.CutPrefix(strings.ToLower(line), "content-length:"); ok {
			length, _ = strconv.Atoi(strings.TrimSpace(v))
		}
	}
	if length < 0 {
		return nil, errors.New("lsp: frame without Content-Length")
	}
	body := make([]byte, length)
	_, err := io.ReadFull(r, body)
	return body, err
}

func (c *conn) write(m message) error {
	body, err := json.Marshal(m)
	if err != nil {
		return err
	}
	c.wmu.Lock()
	defer c.wmu.Unlock()
	if _, err := fmt.Fprintf(c.in, "Content-Length: %d\r\n\r\n", len(body)); err != nil {
		return err
	}
	_, err = c.in.Write(body)
	return err
}

func (c *conn) notify(method string, params any) error {
	raw, err := json.Marshal(params)
	if err != nil {
		return err
	}
	return c.write(message{JSONRPC: "2.0", Method: method, Params: raw})
}

// call sends a request and waits for its response, ctx's deadline, or
// the server's exit.
func (c *conn) call(ctx context.Context, method string, params, result any) error {
	raw, err := json.Marshal(params)
	if err != nil {
		return err
	}
	id := c.next.Add(1)
	ch := make(chan message, 1)
	c.pmu.Lock()
	c.pending[id] = ch
	c.pmu.Unlock()
	if err := c.write(message{JSONRPC: "2.0", ID: jsontext.Value(strconv.FormatInt(id, 10)), Method: method, Params: raw}); err != nil {
		return err
	}
	select {
	case m, ok := <-ch:
		if !ok {
			return c.exitErr()
		}
		if m.Error != nil {
			return m.Error
		}
		if result == nil || len(m.Result) == 0 {
			return nil
		}
		return json.Unmarshal(m.Result, result)
	case <-ctx.Done():
		c.pmu.Lock()
		delete(c.pending, id)
		c.pmu.Unlock()
		_ = c.notify("$/cancelRequest", map[string]any{"id": id})
		return fmt.Errorf("%s: no answer in time", method)
	}
}

func (c *conn) exitErr() error {
	msg := "language server exited"
	if s := strings.TrimSpace(c.stderr.String()); s != "" {
		msg += ": " + s
	}
	return errors.New(msg)
}

func (c *conn) alive() bool {
	select {
	case <-c.done:
		return false
	default:
		return true
	}
}

// close asks the server to shut down, then kills it.
func (c *conn) close() {
	if c.alive() {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		_ = c.call(ctx, "shutdown", nil, nil)
		cancel()
		_ = c.notify("exit", nil)
	}
	_ = c.in.Close()
	select {
	case <-c.done:
	case <-time.After(time.Second):
		_ = c.cmd.Process.Kill()
	}
	_ = c.cmd.Wait()
}

// tailBuffer keeps the last max bytes written: a server's stderr, for
// the error when it dies.
type tailBuffer struct {
	mu  sync.Mutex
	max int
	b   []byte
}

func (t *tailBuffer) Write(p []byte) (int, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.b = append(t.b, p...)
	if over := len(t.b) - t.max; over > 0 {
		t.b = t.b[over:]
	}
	return len(p), nil
}

func (t *tailBuffer) String() string {
	t.mu.Lock()
	defer t.mu.Unlock()
	return string(t.b)
}
