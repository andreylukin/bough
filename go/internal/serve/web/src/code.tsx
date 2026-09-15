import { useMemo } from "react";
import { scriptHead } from "./render";
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

/**
 * A shell command's operation and target, from a small table of
 * commands bough runs often. Anything else is shown as it was run:
 * the line names what the command is, never a guess at why.
 */
const OPS: [RegExp, string, (cmd: string, m: RegExpExecArray) => string][] = [
  [/\brestish\s+exa\s+search\b/, "Search", (c) => quoted(c, "query")],
  [/\brestish\s+exa\s+(?:get-contents|contents)\b/, "Fetch", (c) => /https?:\/\/[^\s"',\]]+/.exec(c)?.[0] ?? ""],
  [/\bgo\s+test\b([^|;&]*)/, "Test", (_, m) => m[1].trim()],
  [/\bgo\s+build\b([^|;&]*)/, "Build", (_, m) => m[1].trim()],
  [/\bgo\s+vet\b([^|;&]*)/, "Vet", (_, m) => m[1].trim()],
];

/** The value of a `query: "…"` or `"query": "…"` argument. */
function quoted(cmd: string, key: string): string {
  const v = new RegExp(`"?${key}"?\\s*:\\s*"([^"]*)"`).exec(cmd)?.[1] ?? "";
  return v.includes("${") ? "" : v; // filled in at run time: not recorded
}

export function describeBash(cmd: string): { verb: string; gist: string } {
  for (const [re, verb, target] of OPS) {
    const m = re.exec(cmd);
    // No literal target (a URL built at run time): the operation alone.
    if (m) return { verb, gist: target(cmd, m) };
  }
  return { verb: "Ran", gist: gistOf(scriptHead(cmd) || cmd) };
}

export function parseCall(code: string): Call {
  const raw = code.trim();
  const call = firstCall(raw);
  const names = [...raw.matchAll(/tools\.(\w+)\s*\(/g)].map((m) => m[1]);
  if (!call || names.length > 1) {
    // Several calls in one block: name the tools it used, not its first
    // line ("const out = []" says nothing). None: show the program itself.
    // What each call was aimed at is what tells two programs apart: the
    // commands and paths, from the recorded source, never guessed.
    if (names.length > 1) {
      const ops = [...raw.matchAll(/tools\.(\w+)\s*\(/g)].map((m) => {
        let i = m.index + m[0].length;
        while (i < raw.length && /\s/.test(raw[i])) i++;
        const s = readString(raw, i);
        return s ? (m[1] === "bash" ? describeBash(s.value) : { verb: "", gist: s.value.split("\n")[0] }) : { verb: "", gist: m[1] };
      });
      const verbs = [...new Set(ops.map((o) => o.verb))];
      const verb = verbs.every(Boolean) ? capped(verbs.map((v, i) => (i ? v.toLowerCase() : v)), 2, " + ") : "Program";
      return { verb, target: "", gist: capped([...new Set(ops.map((o) => o.gist))], 1, " · "), body: raw, lang: "javascript", raw };
    }
    return { verb: "Code", target: "", gist: gistOf(raw), body: raw, lang: "javascript", raw };
  }
  const [a = "", b = ""] = call.args;
  switch (call.name) {
    case "bash":
      return { ...describeBash(a), target: a, body: a, lang: "bash", raw };
    case "write":
      return { verb: "Wrote", target: a, gist: a, body: b || raw, lang: langForPath(a), raw };
    case "patch":
      return { verb: "Patched", target: a, gist: a, body: b || raw, lang: langForPath(a) || "diff", raw };
    case "view":
      return { verb: "Read", target: a, gist: a, body: "", lang: "", raw };
    case "spawn":
    case "spawnAll":
      return { verb: "Delegate", target: "", gist: agents(raw), body: raw, lang: "javascript", raw };
    case "ask":
      return { verb: "Asked you", target: a, gist: a, body: "", lang: "", raw };
    default:
      return { verb: call.name, target: a, gist: gistOf(a || raw), body: raw, lang: "javascript", raw };
  }
}

/** The first `keep` names, then "+N" for the rest: a line names the work, the hover lists it. */
export function capped(names: string[], keep: number, sep: string): string {
  return names.length > keep ? `${names.slice(0, keep).join(sep)} +${names.length - keep}` : names.join(sep);
}

/** "N agents" from the tasks the call lists; nothing when they are built at run time. */
function agents(raw: string): string {
  const n = [...raw.matchAll(/\b(?:task|prompt)\s*:/g)].length || (/\btools\.spawn\s*\(/.test(raw) ? 1 : 0);
  return n ? `${n} ${n === 1 ? "agent" : "agents"}` : "";
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

/**
 * A job tool call read as what it did: "jobWait(177, 15)" is "Waiting for
 * Job 177 · limit 15s", not the call. Null when the program is not a job call.
 */
export function toolCallLabel(code: string): string | null {
  const wait = /tools\.jobWait\(\s*(\d+)\s*(?:,\s*(\d+)\s*)?\)/.exec(code);
  if (wait) return `Waiting for Job ${wait[1]}` + (wait[2] ? ` · limit ${wait[2]}s` : "");
  const job = /tools\.job\(\s*(\d+)\s*\)/.exec(code);
  if (job) return `Read output from Job ${job[1]}`;
  if (/tools\.jobs\(\s*\)/.test(code)) return "Listed jobs";
  return null;
}
