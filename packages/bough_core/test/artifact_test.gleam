import bough_core/artifact.{Artifacts, Edit, Grep, Read, Run, Write}
import gleam/option.{None, Some}

pub fn parses_run_write_check_and_prose_test() {
  let text =
    "I'll set this up now.\n\n"
    <> "### STEP 1: install deps\nRUN\n```sh\napt-get install -y jq\n```\n\n"
    <> "### STEP 2: the program\nWRITE /app/run.py\n```\nprint('hi')\nprint('bye')\n```\n\n"
    <> "### CHECK\n```sh\ntest -f /app/run.py\n```\n"
  let a = artifact.parse(text)
  assert a.prose == "I'll set this up now."
  assert a.steps
    == [
      Run("install deps", "apt-get install -y jq"),
      Write("the program", "/app/run.py", "print('hi')\nprint('bye')"),
    ]
  assert a.check == Some("test -f /app/run.py")
  assert a.done == False
}

pub fn prose_markdown_headings_are_not_steps_test() {
  // The supervisor writes prose with `###` markdown headings and fenced code
  // blocks; these must stay prose, not get parsed (and executed) as RUN steps.
  let text =
    "Codebase Overview\n\n"
    <> "### Structure\n```\nsrc/   compiler\ndocs/  notes\n```\n\n"
    <> "### Commands\n```\ncamp build <file>\n```\n"
  let a = artifact.parse(text)
  assert a.steps == []
  assert a.check == None
}

pub fn parses_edit_step_test() {
  let text =
    "### STEP 1: fix the sign\nEDIT src/math.sh\n"
    <> "```\nadd() { echo $(( $1 - $2 )); }\n```\n"
    <> "```\nadd() { echo $(( $1 + $2 )); }\n```\n"
  let a = artifact.parse(text)
  assert a.steps
    == [
      Edit(
        "fix the sign",
        "src/math.sh",
        "add() { echo $(( $1 - $2 )); }",
        "add() { echo $(( $1 + $2 )); }",
      ),
    ]
}

pub fn parses_read_and_grep_test() {
  let a = artifact.parse("### STEP 1: look\nREAD src/main.rs 10-40\n")
  assert a.steps == [Read("look", "src/main.rs", Some(#(10, 40)))]

  let b = artifact.parse("### STEP 1: find\nGREP fn main(\n")
  assert b.steps == [Grep("find", "fn main(")]

  let c = artifact.parse("### STEP 1: whole\nREAD notes.txt\n")
  assert c.steps == [Read("whole", "notes.txt", None)]
}

pub fn edit_without_two_fences_is_skipped_test() {
  let a = artifact.parse("### STEP 1: bad edit\nEDIT f.txt\n```\nonly search\n```\n")
  assert a.steps == []
}

pub fn pure_prose_reply_test() {
  let a = artifact.parse("The bug is in the parser — line 42 drops the sign.")
  assert a.steps == []
  assert a.check == None
  assert a.prose == "The bug is in the parser — line 42 drops the sign."
}

pub fn detects_done_test() {
  let a = artifact.parse("Everything verified.\n\nDONE")
  assert a.done == True
  assert a.steps == []

  let b = artifact.parse("DONE.\n")
  assert b.done == True

  // DONE mentioned mid-sentence is not a completion signal.
  let c = artifact.parse("when DONE appears alone it finishes")
  assert c.done == False
}

pub fn tolerates_truncated_final_fence_test() {
  let a = artifact.parse("### STEP 1: long write\nWRITE /tmp/x.txt\n```\npartial content")
  assert a.steps == [Write("long write", "/tmp/x.txt", "partial content")]
}

pub fn step_without_fence_is_skipped_test() {
  let a = artifact.parse("### STEP 1: thinking out loud\nno fence here\n")
  assert a.steps == []
}

pub fn check_without_steps_test() {
  let a = artifact.parse("### CHECK\n```sh\ncargo test -q\n```\n")
  assert a.steps == []
  assert a.check == Some("cargo test -q")
}

// Sanity: the Artifacts record is constructible/visible from outside the module.
pub fn artifacts_constructor_visible_test() {
  let a = Artifacts(prose: "", steps: [], check: None, done: False)
  assert a.steps == []
}
