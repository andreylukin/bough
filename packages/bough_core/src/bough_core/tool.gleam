//// The tools the agent may call inside the sandbox (SPEC.md §5).

pub type Tool {
  Bash
  ReadFile
  WriteFile
  EditFile
  Grep
  Glob
  WebFetch
}

pub fn name(tool: Tool) -> String {
  case tool {
    Bash -> "bash"
    ReadFile -> "read"
    WriteFile -> "write"
    EditFile -> "edit"
    Grep -> "grep"
    Glob -> "glob"
    WebFetch -> "webfetch"
  }
}

/// The v1 toolset.
pub fn v1() -> List(Tool) {
  [Bash, ReadFile, WriteFile, EditFile, Grep, Glob, WebFetch]
}
