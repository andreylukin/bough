package ui

// Text pastes into the composer. Line endings are normalised (Windows
// terminals send CR-only newlines in a bracketed paste). A paste that
// is one path to an image on disk (Finder/Explorer drag-drop, quoted or
// backslash-escaped, or a file:// URL) becomes the "[Image #N]"
// placeholder ctrl+v's image paste inserts. A paste taller than the composer or
// longer than pasteCollapseChars collapses to a "[Pasted text #N +L
// lines]" placeholder, as Claude Code, Codex and opencode do, so the
// composer stays usable; the placeholder expands to the full text on
// submit, and deleting it drops the paste. Anything smaller lands as
// typed text, editable in place.

import (
	"fmt"
	"net/url"
	"os"
	"regexp"
	"strconv"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/llm"
)

// pasteCollapseChars is Claude Code's threshold: past it a paste is a
// placeholder even when it fits the composer's rows.
const pasteCollapseChars = 800

// pastePrefix opens every placeholder; the number after it is the
// paste's index in comp.pastes.
const pastePrefix = "[Pasted text #"

// handlePaste routes one bracketed paste; it reports whether it was
// consumed (false: the textarea inserts the content itself).
func (m *model) handlePaste(msg tea.PasteMsg) (bool, tea.Cmd) {
	if m.inspecting {
		return false, nil
	}
	text := strings.ReplaceAll(msg.Content, "\r\n", "\n")
	text = strings.ReplaceAll(text, "\r", "\n")
	trimmed := strings.TrimSpace(text)
	if trimmed == "" {
		return false, nil
	}
	if path := pastedImagePath(trimmed); path != "" {
		m.attachImage(path)
		return true, nil
	}
	lines := strings.Count(trimmed, "\n") + 1
	if lines <= composerMaxLines && len([]rune(trimmed)) <= pasteCollapseChars {
		if text == msg.Content {
			return false, nil
		}
		m.input.InsertString(text)
		m.syncPalette()
		m.layoutComposer()
		return true, nil
	}
	m.comp.pastes = append(m.comp.pastes, text)
	tag := fmt.Sprintf("%s%d", pastePrefix, len(m.comp.pastes))
	if lines > 1 {
		tag += " +" + plural(lines, "line") + "]"
	} else {
		tag += " " + plural(len([]rune(trimmed)), "char") + "]"
	}
	m.comp.pasteTags = append(m.comp.pasteTags, tag)
	m.input.InsertString(tag)
	m.syncPalette()
	m.layoutComposer()
	m.flash = "pasted " + plural(lines, "line") + " · expands when sent · delete the tag to drop it"
	return true, nil
}

// attachImage inserts an "[Image #N]" placeholder for path; on submit
// it becomes "[Image #N: path]", which the loop sends as pixels.
func (m *model) attachImage(path string) {
	if st, err := os.Stat(path); err == nil && st.Size() > llm.MaxImageBytes {
		m.flash = fmt.Sprintf("image too large: %s is %d MB, over the %d MB limit · not attached", path, st.Size()>>20, llm.MaxImageBytes>>20)
		return
	}
	m.comp.images = append(m.comp.images, path)
	m.input.InsertString(fmt.Sprintf("[Image #%d] ", len(m.comp.images)))
	m.syncPalette()
	m.layoutComposer()
	m.flash = "image attached: " + path + " · delete the tag to drop it"
}

// imageTag is a sent "[Image #N: path]" marker; the transcript shows
// it as "[Image #N]".
var imageTag = regexp.MustCompile(`\[Image (#\d+): [^\]\n]+\]`)

// pastedImagePath returns the on-disk image path a pasted line names,
// "" when the paste is not a single existing image file.
func pastedImagePath(s string) string {
	if strings.Contains(s, "\n") {
		return ""
	}
	s = strings.Trim(s, `"'`)
	if strings.HasPrefix(s, "file://") {
		u, err := url.Parse(s)
		if err != nil {
			return ""
		}
		s = u.Path
	} else {
		var b strings.Builder
		for i := 0; i < len(s); i++ {
			if s[i] == '\\' && i+1 < len(s) {
				i++
			}
			b.WriteByte(s[i])
		}
		s = b.String()
	}
	if llm.ImageMIME(s) == "" {
		return ""
	}
	if st, err := os.Stat(s); err != nil || st.IsDir() {
		return ""
	}
	return s
}

// missingImage returns the path of an image the draft still tags whose
// file is gone (deleted since the paste), "" when all are readable.
func (m *model) missingImage(draft string) string {
	for i, p := range m.comp.images {
		if strings.Contains(draft, fmt.Sprintf("[Image #%d]", i+1)) {
			if _, err := os.Stat(p); err != nil {
				return p
			}
		}
	}
	return ""
}

// expandPastes replaces every placeholder still in the draft with its
// text. Placeholders the user deleted are simply not there to expand.
func (m *model) expandPastes(draft string) string {
	for i, p := range m.comp.images {
		tag := fmt.Sprintf("[Image #%d]", i+1)
		draft = strings.ReplaceAll(draft, tag, tag[:len(tag)-1]+": "+p+"]")
	}
	if len(m.comp.pastes) == 0 || !strings.Contains(draft, pastePrefix) {
		return draft
	}
	var b strings.Builder
	for {
		i := strings.Index(draft, pastePrefix)
		if i < 0 {
			break
		}
		// A tag never holds "[", "]" or a newline: one whose "]" was
		// deleted ends before the next of those and stays literal,
		// rather than swallowing text up to a later "]".
		end := strings.IndexAny(draft[i+1:], "[]\n") + 1
		if end == 0 || draft[i+end] != ']' {
			b.WriteString(draft[:i+len(pastePrefix)])
			draft = draft[i+len(pastePrefix):]
			continue
		}
		tag := draft[i : i+end+1]
		num, _, _ := strings.Cut(tag[len(pastePrefix):], " ")
		num = strings.TrimSuffix(num, "]")
		n, err := strconv.Atoi(num)
		b.WriteString(draft[:i])
		if err == nil && n >= 1 && n <= len(m.comp.pastes) && tag == m.comp.pasteTags[n-1] {
			b.WriteString(m.comp.pastes[n-1])
		} else {
			b.WriteString(tag)
		}
		draft = draft[i+end+1:]
	}
	b.WriteString(draft)
	return b.String()
}
