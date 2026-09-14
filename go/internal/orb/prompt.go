package orb

// LocalPromptSection tells a local session why write and patch are
// missing, so the model does not reach for a shell heredoc instead.
// It lives here, not in plugins/tools, because the orb row sets it: a
// tools row that looked up prompt-sections at Apply would reload when
// the loop provides them, re-provide turn-stats, and reload the loop and
// ui mid-startup (headless lost its input and never exited).
const LocalPromptSection = `Session mode: local. This session is read-only on local files: tools.write and tools.patch do not exist, and you must not change files with the shell either.
Use the shell freely to read and to act on remote systems (gh, kubectl, curl, cloud CLIs).
Throwaway files go only under $BOUGH_SCRATCH.
To change code, tell the user this session cannot, and give the exact command: quit and run "bough --project <slug>" (projects live in ~/.bough/projects/<slug>).`
