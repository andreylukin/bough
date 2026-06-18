//// The supervisor's artifact grammar (SPEC.md §5.2): plain text in, executable
//// artifacts out. This is the contract between the supervisor model and the
//// harness — defined once here, independent of provider.
////
//// The supervisor answers in one fixed shape: prose, then `### STEP n` blocks
//// holding one `RUN` / `WRITE` / `EDIT` / `READ` / `GREP` each, optionally a
//// `### CHECK`. Code travels as plain text in fences (structured JSON payloads
//// break on real code's escaping). Parsing is truncation-tolerant: a reply cut
//// off mid-fence still yields the steps it did emit. Ported from tent's
//// `engine/artifact.rs`.
////
//// This module is pure — no IO.

import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string

pub type Step {
  Run(title: String, cmd: String)
  Write(title: String, path: String, content: String)
  /// Surgical in-place edit: replace the single exact occurrence of `search`
  /// with `replace` in `path`. The harness fails the step if `search` is absent
  /// or not unique — far cheaper and safer than rewriting a whole file.
  Edit(title: String, path: String, search: String, replace: String)
  /// Read a file (optionally a line range) with line numbers — the precise way
  /// to see exact bytes before an `Edit`.
  Read(title: String, path: String, range: Option(#(Int, Int)))
  /// Recursive, line-numbered search of the workspace.
  Grep(title: String, pattern: String)
  /// Delegate a self-contained sub-task to a subagent (SPEC §5): a fresh
  /// supervisor run on the same workspace, driven by `task`. The harness runs it
  /// to completion and feeds its result back as this step's output. The human
  /// can jump into the subagent to watch it and send it messages.
  Spawn(title: String, task: String)
}

pub type Artifacts {
  Artifacts(
    /// Conversational text before the first `###` section — the chat reply.
    prose: String,
    steps: List(Step),
    check: Option(String),
    done: Bool,
  )
}

/// The title of any step.
pub fn step_title(step: Step) -> String {
  case step {
    Run(title, ..) -> title
    Write(title, ..) -> title
    Edit(title, ..) -> title
    Read(title, ..) -> title
    Grep(title, ..) -> title
    Spawn(title, ..) -> title
  }
}

pub fn parse(text: String) -> Artifacts {
  let #(prose, sections) = split_sections(text)
  let parsed = list.map(sections, parse_section)
  let steps =
    list.filter_map(parsed, fn(p) {
      case p {
        PStep(s) -> Ok(s)
        _ -> Error(Nil)
      }
    })
  // The last committed `### CHECK` wins (the supervisor may tighten it).
  let check =
    list.fold(parsed, None, fn(acc, p) {
      case p {
        PCheck(Some(c)) -> Some(c)
        _ -> acc
      }
    })
  Artifacts(prose: prose, steps: steps, check: check, done: detect_done(text))
}

/// The first fenced block's trimmed contents, if any. The worker replies with
/// a single ```sh …``` command; this pulls it out (SPEC.md §5.1).
pub fn first_fence(text: String) -> Option(String) {
  case list.first(fences(text)) {
    Ok(f) -> Some(string.trim(f))
    Error(_) -> None
  }
}

// --- Sections ------------------------------------------------------------

type Section {
  Section(header: String, body: String)
}

type Parsed {
  PStep(Step)
  PCheck(Option(String))
  PNothing
}

/// Split into the leading prose and the `###`-delimited sections. A header line
/// starts with `###` followed by whitespace; the header text is the rest of
/// that line, the body is the lines until the next header.
fn split_sections(text: String) -> #(String, List(Section)) {
  do_split(string.split(text, "\n"), [], None, [])
}

fn do_split(
  lines: List(String),
  prose_rev: List(String),
  current: Option(#(String, List(String))),
  sections_rev: List(Section),
) -> #(String, List(Section)) {
  case lines {
    [] -> #(
      string.trim(string.join(list.reverse(prose_rev), "\n")),
      list.reverse(flush(current, sections_rev)),
    )
    [line, ..rest] ->
      case is_header(line) {
        True -> {
          let header = string.trim_start(string.drop_start(line, 3))
          do_split(rest, prose_rev, Some(#(header, [])), flush(current, sections_rev))
        }
        False ->
          case current {
            Some(#(h, body_rev)) ->
              do_split(rest, prose_rev, Some(#(h, [line, ..body_rev])), sections_rev)
            None -> do_split(rest, [line, ..prose_rev], None, sections_rev)
          }
      }
  }
}

fn flush(
  current: Option(#(String, List(String))),
  sections_rev: List(Section),
) -> List(Section) {
  case current {
    Some(#(h, body_rev)) -> [
      Section(h, string.join(list.reverse(body_rev), "\n")),
      ..sections_rev
    ]
    None -> sections_rev
  }
}

/// A section starts only on a recognized artifact header — `### STEP …` or
/// `### CHECK …`. Other `###` lines are ordinary markdown headings the
/// supervisor writes in prose (e.g. `### Structure`), so they must NOT start a
/// section, or their fenced code blocks get parsed as `RUN` steps and executed.
fn is_header(line: String) -> Bool {
  case string.starts_with(line, "### ") || string.starts_with(line, "###\t") {
    False -> False
    True -> {
      let keyword =
        line
        |> string.drop_start(3)
        |> string.trim_start
        |> string.split(" ")
        |> list.first
        |> result.unwrap("")
        |> string.uppercase
      keyword == "STEP" || keyword == "CHECK"
    }
  }
}

fn parse_section(section: Section) -> Parsed {
  case is_check(section.header) {
    True ->
      case list.first(fences(section.body)) {
        Ok(f) -> PCheck(Some(string.trim(f)))
        Error(_) -> PCheck(None)
      }
    False ->
      case parse_step(title_from_header(section.header), section.body) {
        Ok(step) -> PStep(step)
        Error(_) -> PNothing
      }
  }
}

/// Verb precedence mirrors tent: READ, GREP, WRITE, EDIT, else RUN. Once a
/// fenced verb (WRITE/EDIT) matches, a missing fence skips the step — it does
/// not fall through to RUN.
fn parse_step(title: String, body: String) -> Result(Step, Nil) {
  let lines = string.split(body, "\n")
  let fs = fences(body)
  case list.first(list.filter_map(lines, read_of)) {
    Ok(#(p, r)) -> Ok(Read(title, p, r))
    Error(_) ->
      case list.first(list.filter_map(lines, grep_of)) {
        Ok(pat) -> Ok(Grep(title, pat))
        Error(_) ->
          case list.first(list.filter_map(lines, write_path_of)) {
            Ok(path) ->
              case list.first(fs) {
                Ok(f) -> Ok(Write(title, path, f))
                Error(_) -> Error(Nil)
              }
            Error(_) ->
              case list.first(list.filter_map(lines, edit_path_of)) {
                Ok(path) ->
                  case fs {
                    [search, replace, ..] -> Ok(Edit(title, path, search, replace))
                    _ -> Error(Nil)
                  }
                Error(_) ->
                  case list.first(fs) {
                    Ok(f) -> Ok(Run(title, string.trim(f)))
                    Error(_) -> Error(Nil)
                  }
              }
          }
      }
  }
}

// --- Fences --------------------------------------------------------------

/// Fenced-block contents in order. Splitting on ``` makes the odd-indexed
/// chunks the insides of fences; each one's first line is the (ignored)
/// language tag, and a single trailing newline belongs to the closing fence.
/// An unterminated final fence still yields its partial content.
fn fences(body: String) -> List(String) {
  string.split(body, "```")
  |> list.index_map(fn(tok, i) { #(i, tok) })
  |> list.filter(fn(p) { is_odd(p.0) })
  |> list.map(fn(p) { fence_content(p.1) })
}

fn fence_content(tok: String) -> String {
  let content = case string.split_once(tok, "\n") {
    Ok(#(_lang, rest)) -> rest
    Error(_) -> ""
  }
  case string.ends_with(content, "\n") {
    True -> string.drop_end(content, 1)
    False -> content
  }
}

fn is_odd(i: Int) -> Bool {
  i - i / 2 * 2 == 1
}

// --- Verb lines ----------------------------------------------------------

fn read_of(line: String) -> Result(#(String, Option(#(Int, Int))), Nil) {
  let t = string.trim(line)
  case string.starts_with(t, "READ ") {
    False -> Error(Nil)
    True -> {
      let toks =
        string.drop_start(t, 5)
        |> string.trim
        |> string.split(" ")
        |> list.filter(fn(x) { x != "" })
      case toks {
        [path] -> Ok(#(path, None))
        [path, r] ->
          case parse_range(r) {
            Ok(rng) -> Ok(#(path, Some(rng)))
            Error(_) -> Error(Nil)
          }
        _ -> Error(Nil)
      }
    }
  }
}

fn parse_range(r: String) -> Result(#(Int, Int), Nil) {
  case string.split_once(r, "-") {
    Ok(#(a, b)) ->
      case int.parse(a), int.parse(b) {
        Ok(x), Ok(y) -> Ok(#(x, y))
        _, _ -> Error(Nil)
      }
    Error(_) -> Error(Nil)
  }
}

fn grep_of(line: String) -> Result(String, Nil) {
  let t = string.trim(line)
  case string.starts_with(t, "GREP ") {
    True ->
      case string.trim(string.drop_start(t, 5)) {
        "" -> Error(Nil)
        pat -> Ok(pat)
      }
    False -> Error(Nil)
  }
}

fn write_path_of(line: String) -> Result(String, Nil) {
  single_path(line, "WRITE ", 6)
}

fn edit_path_of(line: String) -> Result(String, Nil) {
  single_path(line, "EDIT ", 5)
}

/// A verb line carrying exactly one whitespace-free path and nothing else.
fn single_path(line: String, prefix: String, drop: Int) -> Result(String, Nil) {
  let t = string.trim(line)
  case string.starts_with(t, prefix) {
    True ->
      case string.trim(string.drop_start(t, drop)) {
        "" -> Error(Nil)
        rest ->
          case string.contains(rest, " ") {
            True -> Error(Nil)
            False -> Ok(rest)
          }
      }
    False -> Error(Nil)
  }
}

// --- Titles & DONE -------------------------------------------------------

fn is_check(header: String) -> Bool {
  string.starts_with(string.uppercase(string.trim(header)), "CHECK")
}

/// Strip a leading `STEP n:` from a section header, leaving the human title.
fn title_from_header(header: String) -> String {
  let h = string.trim_start(header)
  case string.starts_with(h, "STEP") {
    False -> string.trim(header)
    True ->
      h
      |> string.drop_start(4)
      |> string.trim_start
      |> drop_leading_digits
      |> string.trim_start
      |> drop_leading_colon
      |> string.trim
  }
}

fn drop_leading_digits(s: String) -> String {
  case string.pop_grapheme(s) {
    Ok(#(c, rest)) ->
      case is_digit(c) {
        True -> drop_leading_digits(rest)
        False -> s
      }
    Error(_) -> s
  }
}

fn is_digit(c: String) -> Bool {
  case c {
    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" -> True
    _ -> False
  }
}

fn drop_leading_colon(s: String) -> String {
  case string.starts_with(s, ":") {
    True -> string.drop_start(s, 1)
    False -> s
  }
}

/// `DONE` alone on a line (optionally `.`/`!`) is the completion signal; the
/// word mid-sentence is not.
fn detect_done(text: String) -> Bool {
  list.any(string.split(text, "\n"), is_done_line)
}

fn is_done_line(line: String) -> Bool {
  let t = string.trim(line)
  case string.starts_with(t, "DONE") {
    True ->
      case string.trim(string.drop_start(t, 4)) {
        "" | "." | "!" -> True
        _ -> False
      }
    False -> False
  }
}
