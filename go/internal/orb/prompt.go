package orb

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// LocalPromptSection tells a local session why write and patch are
// missing, so the model does not reach for a shell heredoc instead.
// It lives here, not in plugins/tools, because the orb row sets it: a
// tools row that looked up prompt-sections at Apply would reload when
// the loop provides them, re-provide turn-stats, and reload the loop and
// ui mid-startup (headless lost its input and never exited).
const LocalPromptSection = `Session mode: local. This session is read-only on local files: tools.write and tools.patch do not exist, and you must not change files with the shell either.
Use the shell freely to read and to act on remote systems (gh, kubectl, curl, cloud CLIs).
Throwaway files go only under $BOUGH_SCRATCH.
Project definitions (repos, checks, env, resume.sh) are the one thing you may change, and only with "bough project" (run it with no args for usage); it validates every change.
To change code, tell the user this session cannot, and give the exact command: quit and run "bough --project <slug>" (projects live in ~/.bough/projects/<slug>).`

// WriteRootsEnv lists the only directories a local session may write,
// for a job bough itself starts to maintain its own files: the wiki
// ingest runs headless in local mode and, read-only, refused every run
// and re-ingested the same sessions every five minutes. Unset for every
// session a person starts, which stays read-only.
const WriteRootsEnv = "BOUGH_WRITE_ROOTS"

// LocalWriteRoots reads WriteRootsEnv: absolute paths, cleaned; relative
// or empty entries are dropped.
func LocalWriteRoots() []string {
	var roots []string
	for _, r := range filepath.SplitList(os.Getenv(WriteRootsEnv)) {
		if filepath.IsAbs(r) {
			roots = append(roots, filepath.Clean(r))
		}
	}
	return roots
}

// LocalPromptSectionFor is LocalPromptSection, or, when the session has
// write roots, the note that says where tools.write and tools.patch work.
func LocalPromptSectionFor(roots []string) string {
	if len(roots) == 0 {
		return LocalPromptSection
	}
	return fmt.Sprintf(`Session mode: local, with write access to %s only. tools.write and tools.patch work there and refuse any other path; everything else is read-only, and you must not change files outside those directories with the shell either.
Use the shell freely to read and to act on remote systems (gh, kubectl, curl, cloud CLIs).
Throwaway files go only under $BOUGH_SCRATCH.`, strings.Join(roots, ", "))
}
