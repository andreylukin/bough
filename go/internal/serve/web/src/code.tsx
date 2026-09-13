import { useMemo } from "react";
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import diff from "highlight.js/lib/languages/diff";
import python from "highlight.js/lib/languages/python";
import go from "highlight.js/lib/languages/go";
import typescript from "highlight.js/lib/languages/typescript";
import yaml from "highlight.js/lib/languages/yaml";

/**
 * Only the languages that actually appear in a transcript are registered:
 * the full highlight.js is ~1MB and the bundle ships inside the binary.
 */
for (const [name, lang] of Object.entries({ bash, javascript, json, diff, python, go, typescript, yaml })) {
  hljs.registerLanguage(name, lang as never);
}

const BY_EXT: Record<string, string> = {
  js: "javascript", mjs: "javascript", jsx: "javascript",
  ts: "typescript", tsx: "typescript",
  py: "python", go: "go", json: "json",
  yml: "yaml", yaml: "yaml",
  sh: "bash", bash: "bash", zsh: "bash",
};

/** The language to colour a file's contents with, from its name. */
export function langForPath(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return BY_EXT[ext] ?? "";
}

/**
 * A tool call as a person reads it. 83% of the code blocks in a real
 * transcript are `console.log(tools.bash("<command>"))` — a shell
 * command wearing a JavaScript costume. Showing the costume is what
 * made these unreadable, so the call is unwrapped and the thing inside
 * is shown in its own language.
 *
 * This is also why a general JS formatter is the wrong tool: prettier
 * never breaks a string literal, so it would reformat the wrapper and
 * leave the actual command exactly as unreadable as it was.
 */
export interface Call {
  verb: string;   // Ran, Wrote, Patched, Read, …
  target: string; // the command, or the path
  gist: string;   // the collapsed one-line preview
  body: string;   // what to show in the block
  lang: string;   // how to colour it
  raw: string;    // the original program, still reachable
}

/**
 * The part of a command worth showing in one line.
 *
 * Agents working in a worktree prefix nearly every command with
 * `cd <very long branch-scoped path> && …`. That prefix is identical
 * on every row of a session, so it is the one part carrying no
 * information — and it was consuming the whole preview, leaving
 * "git commit" and "cat somefile" looking exactly alike. The directory
 * is still there when the block is opened.
 */
export function gistOf(cmd: string): string {
  const m = /^\s*cd\s+(?:"[^"]*"|'[^']*'|[^\s;&|]+)\s*(?:&&|;)\s*/.exec(cmd);
  const rest = m ? cmd.slice(m[0].length) : cmd;
  return (rest.trim() || cmd.trim()).split("\n")[0];
}

/** Read one JS string literal starting at `from` (the quote). */
function readString(src: string, from: number): { value: string; end: number } | null {
  const quote = src[from];
  if (quote !== '"' && quote !== "'" && quote !== "`") return null;
  let out = "";
  for (let i = from + 1; i < src.length; i++) {
    const c = src[i];
    if (c === "\\") {
      const n = src[i + 1];
      out += n === "n" ? "\n" : n === "t" ? "\t" : n === "\\" ? "\\" : n ?? "";
      i++;
      continue;
    }
    if (c === quote) return { value: out, end: i };
    out += c;
  }
  return null;
}

/** The first `tools.<name>(` call and its first two string arguments. */
function firstCall(src: string): { name: string; args: string[] } | null {
  const m = /tools\.(\w+)\s*\(/.exec(src);
  if (!m) return null;
  const args: string[] = [];
  let i = m.index + m[0].length;
  for (let n = 0; n < 2 && i < src.length; n++) {
    while (i < src.length && /[\s,]/.test(src[i])) i++;
    const s = readString(src, i);
    if (!s) break;
    args.push(s.value);
    i = s.end + 1;
  }
  return { name: m[1], args };
}

export function parseCall(code: string): Call {
  const raw = code.trim();
  const call = firstCall(raw);
  const names = [...raw.matchAll(/tools\.(\w+)\s*\(/g)].map((m) => m[1]);
  if (!call || names.length > 1) {
    // Several calls in one block: name the tools it used, not its first
    // line ("const out = []" says nothing). None: show the program itself.
    if (names.length > 1) return { verb: "Program", target: "", gist: [...new Set(names)].join(", "), body: raw, lang: "javascript", raw };
    return { verb: "Code", target: "", gist: gistOf(raw), body: raw, lang: "javascript", raw };
  }
  const [a = "", b = ""] = call.args;
  switch (call.name) {
    case "bash":
      return { verb: "Ran", target: a, gist: gistOf(a), body: a, lang: "bash", raw };
    case "write":
      return { verb: "Wrote", target: a, gist: a, body: b || raw, lang: langForPath(a), raw };
    case "patch":
      return { verb: "Patched", target: a, gist: a, body: b || raw, lang: langForPath(a) || "diff", raw };
    case "view":
      return { verb: "Read", target: a, gist: a, body: "", lang: "", raw };
    case "spawn":
    case "spawnAll":
      return { verb: "Spawned subagents", target: "", gist: gistOf(raw), body: raw, lang: "javascript", raw };
    case "ask":
      return { verb: "Asked you", target: a, gist: a, body: "", lang: "", raw };
    default:
      return { verb: call.name, target: a, gist: gistOf(a || raw), body: raw, lang: "javascript", raw };
  }
}

/** Highlighted source. Falls back to plain text when the language is unknown. */
export function Code({ text, lang }: { text: string; lang: string }) {
  const html = useMemo(() => {
    if (!lang || !hljs.getLanguage(lang)) return null;
    try {
      return hljs.highlight(text, { language: lang, ignoreIllegals: true }).value;
    } catch {
      return null;
    }
  }, [text, lang]);
  if (html === null) return <pre className="mono hl">{text}</pre>;
  return <pre className="mono hl" dangerouslySetInnerHTML={{ __html: html }} />;
}
