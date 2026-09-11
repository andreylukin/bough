package llm

import (
	"encoding/base64"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

// imageRef is how an image rides in message text: "[Image #1: path]"
// from a composer paste, "[Image: path]" from tools.view. The path
// stays in history (small, survives resume); providers read the
// pixels only when they build a request.
var imageRef = regexp.MustCompile(`\[Image(?: #\d+)?: ([^\]\n]+)\]`)

// ImageRefs returns the image paths text references, in order.
func ImageRefs(text string) []string {
	var paths []string
	for _, m := range imageRef.FindAllStringSubmatch(text, -1) {
		paths = append(paths, m[1])
	}
	return paths
}

// maxImageBytes is Anthropic's per-image limit; larger files are
// skipped rather than failing the whole request.
const maxImageBytes = 5 << 20

type image struct{ mime, data string }

func (i image) dataURL() string { return "data:" + i.mime + ";base64," + i.data }

// ImageMIME is the media type every provider accepts for path, "" for
// anything else.
func ImageMIME(path string) string {
	switch strings.ToLower(filepath.Ext(path)) {
	case ".png":
		return "image/png"
	case ".jpg", ".jpeg":
		return "image/jpeg"
	case ".gif":
		return "image/gif"
	case ".webp":
		return "image/webp"
	}
	return ""
}

// loadImages reads the images a message references; unreadable,
// oversized or unsupported ones are dropped (the marker text stays).
func loadImages(paths []string) []image {
	var out []image
	for _, p := range paths {
		mime := ImageMIME(p)
		if mime == "" {
			continue
		}
		data, err := os.ReadFile(p)
		if err != nil || len(data) > maxImageBytes {
			continue
		}
		out = append(out, image{mime, base64.StdEncoding.EncodeToString(data)})
	}
	return out
}
