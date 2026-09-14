package serve

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/andreylukin/bough/plugins/llm"
)

// An image pasted into the web composer is saved where the TUI's ctrl+v
// saves one, ~/.bough/attachments, and the composer sends its path as
// "[Image #N: path]", which the llm plugin turns into pixels. The same
// directory is the only place GET serves images from, so the transcript
// can show what was sent without exposing the rest of the disk.

var imageExt = map[string]string{
	"image/png":  ".png",
	"image/jpeg": ".jpg",
	"image/gif":  ".gif",
	"image/webp": ".webp",
}

func (a *API) attachDir() string { return filepath.Join(a.home, ".bough", "attachments") }

func (a *API) upload(w http.ResponseWriter, r *http.Request) {
	ext, ok := imageExt[strings.TrimSpace(strings.Split(r.Header.Get("Content-Type"), ";")[0])]
	if !ok {
		writeErr(w, http.StatusUnsupportedMediaType, fmt.Errorf("serve: api: attachment must be png, jpeg, gif or webp"))
		return
	}
	data, err := io.ReadAll(http.MaxBytesReader(w, r.Body, llm.MaxImageBytes))
	if err != nil {
		writeErr(w, http.StatusRequestEntityTooLarge, fmt.Errorf("serve: api: image over the %d MB limit", llm.MaxImageBytes>>20))
		return
	}
	if len(data) == 0 {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: empty attachment"))
		return
	}
	dir := a.attachDir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: %w", err))
		return
	}
	f, err := os.CreateTemp(dir, time.Now().Format("20060102-150405.000")+"-*"+ext)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: %w", err))
		return
	}
	_, err = f.Write(data)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: %w", err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"path": f.Name()})
}

// maxFileBytes caps a non-image file dropped or pasted into the composer.
const maxFileBytes = 64 << 20

// uploadFile saves any other file into the session's scratchpad,
// ~/.bough/scratch/<id>, which the scratchpad plugin names after the
// session's history file; the prompt then carries its path.
func (a *API) uploadFile(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	name := filepath.Base(filepath.Clean("/" + r.URL.Query().Get("name")))
	if name == "/" || name == "." {
		name = "file"
	}
	data, err := io.ReadAll(http.MaxBytesReader(w, r.Body, maxFileBytes))
	if err != nil {
		writeErr(w, http.StatusRequestEntityTooLarge, fmt.Errorf("serve: api: file over the %d MB limit", maxFileBytes>>20))
		return
	}
	dir := filepath.Join(a.home, ".bough", "scratch", id)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: %w", err))
		return
	}
	// A second drop of the same name keeps both.
	ext := filepath.Ext(name)
	path := filepath.Join(dir, name)
	for i := 2; ; i++ {
		f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o644)
		if os.IsExist(err) {
			path = filepath.Join(dir, fmt.Sprintf("%s-%d%s", strings.TrimSuffix(name, ext), i, ext))
			continue
		}
		if err == nil {
			_, err = f.Write(data)
			if cerr := f.Close(); err == nil {
				err = cerr
			}
		}
		if err != nil {
			writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: %w", err))
			return
		}
		break
	}
	writeJSON(w, http.StatusOK, map[string]any{"path": path})
}

func (a *API) attachment(w http.ResponseWriter, r *http.Request) {
	p := filepath.Clean(r.URL.Query().Get("path"))
	if filepath.Dir(p) != a.attachDir() || llm.ImageMIME(p) == "" {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: not an attachment"))
		return
	}
	w.Header().Set("Content-Type", llm.ImageMIME(p))
	http.ServeFile(w, r, p)
}
