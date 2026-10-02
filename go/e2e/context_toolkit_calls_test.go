//go:build !windows

package e2e

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/contextkit"
)

const toolkitOriginal = "TOOLKIT_ORIGINAL_REQUEST"
const toolkitHeader = "revised notes α\n"
const toolkitExcerpt = "archive this exact text\n"
const toolkitTail = "retained tail\n"
const toolkitDone = "TOOLKIT_PROTOCOL_DONE"

type toolkitRequest struct {
	Tools []struct {
		Name string `json:"name"`
	} `json:"tools"`
	Input []struct {
		Type   string          `json:"type"`
		CallID string          `json:"call_id"`
		Output json.RawMessage `json:"output"`
	} `json:"input"`
}

// Read actual native results, rather than fabricating revisions or peeking at
// private files to choose the next call. The wire output may be text or parts.
func toolkitCallResult(req toolkitRequest, id string, out any) error {
	for _, it := range req.Input {
		if it.Type != "function_call_output" || it.CallID != id {
			continue
		}
		var text string
		if err := json.Unmarshal(it.Output, &text); err != nil {
			var parts []struct {
				Text string `json:"text"`
			}
			if err := json.Unmarshal(it.Output, &parts); err != nil {
				return err
			}
			for _, part := range parts {
				text += part.Text
			}
		}
		// Failed native calls prepend their ordinary error before the row's
		// machine-readable JSON. The conflict contains no editable text.
		at := strings.IndexByte(text, '{')
		if at < 0 {
			return fmt.Errorf("%s returned no JSON: %q", id, text)
		}
		if err := json.Unmarshal([]byte(text[at:]), out); err != nil {
			return fmt.Errorf("%s result %q: %w", id, text, err)
		}
		return nil
	}
	return fmt.Errorf("missing result for %s", id)
}

type toolkitProvider struct {
	mu         sync.Mutex
	step       int
	errors     []error
	initial    contextkit.Info
	edited     contextkit.EditResult
	offloaded  contextkit.OffloadResult
	restored   contextkit.EditResult
	snapshot   contextkit.Info
	finalNotes string
}

func (p *toolkitProvider) next(req toolkitRequest) (string, any, error) {
	decode := func(id string, out any) error { return toolkitCallResult(req, id, out) }
	notes := toolkitHeader + toolkitExcerpt + toolkitTail
	edit := func(revision string, dry bool) contextkit.EditRequest {
		return contextkit.EditRequest{ExpectedRevision: revision, Edits: []contextkit.Edit{{Start: 0, End: p.initial.Bytes, Text: notes}}, DryRun: dry}
	}
	switch p.step {
	case 0:
		present := map[string]bool{}
		for _, tool := range req.Tools {
			present[tool.Name] = true
		}
		for _, name := range []string{"context_inspect", "context_read", "context_search", "context_edit", "context_offload", "context_restore"} {
			if !present[name] {
				return "", nil, fmt.Errorf("real row did not expose %s", name)
			}
		}
		return "context_inspect", struct{}{}, nil
	case 1:
		if e := decode("toolkit-0", &p.initial); e != nil {
			return "", nil, e
		}
		if p.initial.Revision == "" || p.initial.SnapshotID == "" || p.initial.Bytes == 0 {
			return "", nil, fmt.Errorf("incomplete inspect result: %+v", p.initial)
		}
		return "context_read", contextkit.ReadRequest{SnapshotID: p.initial.SnapshotID}, nil
	case 2:
		var r contextkit.ReadResult
		if e := decode("toolkit-1", &r); e != nil {
			return "", nil, e
		}
		if r.Revision != p.initial.Revision || !strings.Contains(r.Text, toolkitOriginal) {
			return "", nil, fmt.Errorf("read lost initial snapshot: %+v", r)
		}
		return "context_search", contextkit.SearchRequest{SnapshotID: p.initial.SnapshotID, Query: toolkitOriginal}, nil
	case 3:
		var r contextkit.SearchResult
		if e := decode("toolkit-2", &r); e != nil {
			return "", nil, e
		}
		if r.Revision != p.initial.Revision || len(r.Matches) == 0 {
			return "", nil, fmt.Errorf("search lost initial snapshot: %+v", r)
		}
		return "context_edit", edit(p.initial.Revision, true), nil
	case 4:
		var r contextkit.EditResult
		if e := decode("toolkit-3", &r); e != nil {
			return "", nil, e
		}
		if !r.DryRun || !r.Changed || r.Revision != p.initial.Revision || r.Bytes != len(notes) {
			return "", nil, fmt.Errorf("bad dry run: %+v", r)
		}
		return "context_inspect", struct{}{}, nil
	case 5:
		var r contextkit.Info
		if e := decode("toolkit-4", &r); e != nil {
			return "", nil, e
		}
		if r.Revision != p.initial.Revision || r.ContentHash != p.initial.ContentHash || r.Bytes != p.initial.Bytes {
			return "", nil, fmt.Errorf("dry run or its own protocol changed notes: %+v", r)
		}
		return "context_edit", edit(r.Revision, false), nil
	case 6:
		if e := decode("toolkit-5", &p.edited); e != nil {
			return "", nil, e
		}
		if p.edited.Revision == "" || p.edited.Revision == p.initial.Revision || p.edited.DryRun || p.edited.Bytes != len(notes) {
			return "", nil, fmt.Errorf("edit did not commit: %+v", p.edited)
		}
		return "context_edit", contextkit.EditRequest{ExpectedRevision: p.initial.Revision, Edits: []contextkit.Edit{{Start: 0, End: 0, Text: "STALE_WRITE_MUST_NOT_LAND"}}}, nil
	case 7:
		var r struct {
			Error    string `json:"error"`
			Expected string `json:"expected_revision"`
			Current  string `json:"current_revision"`
		}
		if e := decode("toolkit-6", &r); e != nil {
			return "", nil, e
		}
		if r.Error != "revision_conflict" || r.Expected != p.initial.Revision || r.Current != p.edited.Revision {
			return "", nil, fmt.Errorf("stale edit did not fail with current revision: %+v", r)
		}
		return "context_offload", contextkit.OffloadRequest{ExpectedRevision: p.edited.Revision, Start: len(toolkitHeader), End: len(toolkitHeader) + len(toolkitExcerpt)}, nil
	case 8:
		if e := decode("toolkit-7", &p.offloaded); e != nil {
			return "", nil, e
		}
		if p.offloaded.ArchiveID == "" || p.offloaded.Revision == p.edited.Revision || p.offloaded.ArchivedBytes != len(toolkitExcerpt) {
			return "", nil, fmt.Errorf("bad offload: %+v", p.offloaded)
		}
		return "context_inspect", struct{}{}, nil
	case 9:
		if e := decode("toolkit-8", &p.snapshot); e != nil {
			return "", nil, e
		}
		if p.snapshot.Revision != p.offloaded.Revision {
			return "", nil, fmt.Errorf("offload revision changed before read")
		}
		return "context_read", contextkit.ReadRequest{SnapshotID: p.snapshot.SnapshotID}, nil
	case 10:
		var r contextkit.ReadResult
		if e := decode("toolkit-9", &r); e != nil {
			return "", nil, e
		}
		want := toolkitHeader + "[[clm-archive:" + p.offloaded.ArchiveID + "]]" + toolkitTail
		if r.Text != want || r.HasMore {
			return "", nil, fmt.Errorf("offload lost notes or marker: %q", r.Text)
		}
		p.finalNotes = want + toolkitExcerpt
		return "context_restore", contextkit.RestoreRequest{ExpectedRevision: r.Revision, ArchiveID: p.offloaded.ArchiveID, Offset: r.Bytes}, nil
	case 11:
		if e := decode("toolkit-10", &p.restored); e != nil {
			return "", nil, e
		}
		if p.restored.Revision == p.offloaded.Revision || p.restored.Bytes != len(p.finalNotes) {
			return "", nil, fmt.Errorf("bad restore: %+v", p.restored)
		}
		return "context_inspect", struct{}{}, nil
	case 12:
		if e := decode("toolkit-11", &p.snapshot); e != nil {
			return "", nil, e
		}
		if p.snapshot.Revision != p.restored.Revision {
			return "", nil, fmt.Errorf("restore revision changed before read")
		}
		return "context_read", contextkit.ReadRequest{SnapshotID: p.snapshot.SnapshotID}, nil
	case 13:
		var r contextkit.ReadResult
		if e := decode("toolkit-12", &r); e != nil {
			return "", nil, e
		}
		if r.Text != p.finalNotes || r.HasMore {
			return "", nil, fmt.Errorf("restore lost intervening notes or excerpt: %q", r.Text)
		}
		return "", nil, nil
	default:
		return "", nil, fmt.Errorf("unexpected provider request after completion")
	}
}

func (p *toolkitProvider) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if r.Method != http.MethodPost || r.URL.Path != "/v1/responses" {
		p.errors = append(p.errors, fmt.Errorf("unexpected provider route %s %s", r.Method, r.URL.Path))
		http.NotFound(w, r)
		return
	}
	var req toolkitRequest
	err := json.NewDecoder(io.LimitReader(r.Body, 4<<20)).Decode(&req)
	var name string
	var args any
	if err == nil {
		name, args, err = p.next(req)
	}
	var output map[string]any
	if err != nil || name == "" {
		text := toolkitDone
		if err != nil {
			p.errors = append(p.errors, fmt.Errorf("step %d: %w", p.step, err))
			text = "TOOLKIT_PROTOCOL_FAILED"
		}
		output = map[string]any{"type": "message", "id": fmt.Sprintf("msg_%d", p.step), "role": "assistant", "status": "completed", "content": []any{map[string]any{"type": "output_text", "text": text, "annotations": []any{}}}}
	} else {
		encoded, _ := json.Marshal(args)
		output = map[string]any{"type": "function_call", "id": fmt.Sprintf("fc_%d", p.step), "call_id": fmt.Sprintf("toolkit-%d", p.step), "name": name, "arguments": string(encoded), "status": "completed"}
	}
	event := map[string]any{"type": "response.completed", "response": map[string]any{"id": fmt.Sprintf("response_%d", p.step), "status": "completed", "output": []any{output}, "usage": map[string]any{"input_tokens": 100, "output_tokens": 10, "total_tokens": 110}}}
	p.step++
	encoded, _ := json.Marshal(event)
	w.Header().Set("Content-Type", "text/event-stream")
	fmt.Fprintf(w, "data: %s\n\n", encoded)
}

// The real engine, real tool row, and real native-call bridge exchange dynamic
// revisions end to end. The only provider is this test's loopback HTTP server.
func TestContextToolkitNativeProtocol(t *testing.T) {
	t.Parallel()
	provider := &toolkitProvider{}
	server := httptest.NewServer(provider)
	defer server.Close()
	// Session naming uses the same provider when llm-small is absent; keep
	// this fixture dedicated to the conversation's native-tool protocol.
	home, cwd, _ := sandbox(t, launchOpts{config: "- id: session-title\n  disabled: true\n"})
	scratch := filepath.Join(cwd, "scratch")
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, boughBin, "--config", "bough.yml", "--headless",
		"--set", "loop.plugin=engine-clm", "--set", "loop.max_steps=30",
		"--set", "llm.plugin=llm-ollama", "--set", "llm.model=toolkit-offline",
		"--set", "llm.base_url="+server.URL+"/v1",
		"--set", "scratchpad.dir="+scratch)
	cmd.Dir = cwd
	cmd.Stdin = strings.NewReader(toolkitOriginal + "\n")
	// Scope write permission only to this test's cwd; never inherit a real
	// user's write roots or scratch location into the process under test.
	for _, v := range env(home) {
		if !strings.HasPrefix(v, "BOUGH_WRITE_ROOTS=") && !strings.HasPrefix(v, "BOUGH_SCRATCH=") {
			cmd.Env = append(cmd.Env, v)
		}
	}
	cmd.Env = append(cmd.Env, "BOUGH_WRITE_ROOTS="+cwd)
	output, err := cmd.CombinedOutput()
	provider.mu.Lock()
	defer provider.mu.Unlock()
	if err != nil || len(provider.errors) != 0 || provider.step != 14 {
		t.Fatalf("protocol: exit=%v requests=%d errors=%v\n%s", err, provider.step, provider.errors, output)
	}
	mustContain(t, string(output), toolkitDone)
	mustNotContain(t, string(output), "TOOLKIT_PROTOCOL_FAILED")
	entries := readEntries(t, onlySession(t, home))
	if len(kinds(entries, "call")) != 13 {
		t.Fatalf("native calls missing/duplicated:\n%s", dump(entries))
	}
	if inputs := kinds(entries, "input"); len(inputs) != 1 || inputs[0].Data["text"] != toolkitOriginal {
		t.Fatalf("editable operations changed canonical input:\n%s", dump(entries))
	}
	paths, err := filepath.Glob(filepath.Join(scratch, ".bough-clm", "*.md"))
	if err != nil || len(paths) != 1 {
		t.Fatalf("context files=%v error=%v", paths, err)
	}
	body, err := os.ReadFile(paths[0])
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(string(body), provider.finalNotes) || !strings.Contains(string(body), toolkitDone) || strings.Contains(string(body), "STALE_WRITE_MUST_NOT_LAND") {
		t.Fatalf("wrong final editable context: %q", body)
	}
}
