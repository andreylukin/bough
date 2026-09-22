import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Back } from "./app";
import { useCopied } from "./loading";

/*
 * Rewrite: retype an AI-written document one sentence at a time, in your
 * own words. The model never writes into the text. Beside the box you
 * type in it shows, for the sentence you are on, the claim it makes, the
 * phrases that read as machine prose, and what to do with it.
 *
 * The document is split here, in the page: headings, code fences and
 * bullets keep their shape, prose paragraphs become sentences. Progress
 * lives in localStorage so a reload lands on the same sentence.
 */

export type Unit = {
  /** text: a sentence the person rewrites. struct: a heading, fence or blank the page keeps as is. */
  kind: "text" | "struct";
  text: string;
  /** Paragraph the unit belongs to; sentences of one paragraph join back with a space. */
  para: number;
  /** A list marker ("- ", "1. ", "  - ") that leads the rewritten line. */
  prefix?: string;
};

export type Version = { text: string; action: "rewrite" | "keep" | "cut" };
export type Note = { claim: string; tells: string[]; advice: string };

const ABBR = /\b(e\.g|i\.e|etc|vs|cf|Mr|Mrs|Ms|Dr|Prof|St|Jr|Sr|No|Fig|approx|dept|est|min|max|ver|v)\.$/i;

/** A paragraph's sentences: split after . ! ? (plus a closing quote or bracket) before a capital, digit or opener; abbreviations and decimals stay whole. */
export function sentences(para: string): string[] {
  const out: string[] = [];
  let start = 0;
  const re = /[.!?]+["'”’)\]]*\s+(?=["'“‘(\[A-Z0-9])/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(para))) {
    const head = para.slice(start, m.index + m[0].trimEnd().length);
    const word = head.slice(0, head.search(/["'”’)\]]*$/)).replace(/[.!?]+$/, "");
    if (ABBR.test(word + ".") || /\d$/.test(word) && /^\d/.test(para.slice(re.lastIndex))) continue;
    out.push(head.trim());
    start = re.lastIndex;
  }
  const tail = para.slice(start).trim();
  if (tail) out.push(tail);
  return out;
}

/** The document as units, in order. */
export function split(text: string): Unit[] {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const units: Unit[] = [];
  let para = 0;
  let buf: string[] = [];
  const flush = () => {
    if (!buf.length) return;
    for (const s of sentences(buf.join(" "))) units.push({ kind: "text", text: s, para });
    buf = [];
    para++;
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*(```|~~~)/.test(line)) {
      flush();
      const fence = [line];
      const close = line.trim().slice(0, 3);
      for (i++; i < lines.length; i++) { fence.push(lines[i]); if (lines[i].trim().startsWith(close)) break; }
      units.push({ kind: "struct", text: fence.join("\n"), para: para++ });
      continue;
    }
    if (!line.trim()) { flush(); continue; }
    if (/^\s*#{1,6}\s/.test(line) || /^\s*(---|\*\*\*|___)\s*$/.test(line) || /^\s*\|/.test(line)) {
      flush();
      units.push({ kind: "struct", text: line, para: para++ });
      continue;
    }
    const li = /^(\s*(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?)(.*)$/.exec(line);
    if (li) {
      flush();
      // A bullet is one unit: bullets are meant to be one thought, and a
      // second sentence in one is itself something to notice.
      units.push({ kind: "text", text: li[2].trim(), para: para++, prefix: li[1] });
      continue;
    }
    const q = /^(\s*>\s?)(.*)$/.exec(line);
    if (q) { flush(); units.push({ kind: "text", text: q[2].trim(), para: para++, prefix: q[1] }); continue; }
    buf.push(line.trim());
  }
  flush();
  return units;
}

/** The rewritten document. A kept sentence is the original; a cut one is gone; an unvisited one is the original too, so a half-done pass still reads. */
export function assemble(units: Unit[], versions: Record<number, Version>): string {
  const out: string[] = [];
  let lastPara = -1;
  let lastPrefixed = false;
  let line: string[] = [];
  const endLine = () => { if (line.length) out.push(line.join(" ")); line = []; };
  units.forEach((u, i) => {
    if (u.para !== lastPara) {
      endLine();
      // List items and quote lines follow each other without a blank line.
      if (lastPara >= 0 && !(lastPrefixed && u.prefix)) out.push("");
      lastPara = u.para; lastPrefixed = !!u.prefix;
    }
    if (u.kind === "struct") { line.push(u.text); return; }
    const v = versions[i];
    const text = !v || v.action === "keep" ? u.text : v.action === "cut" ? "" : v.text.trim();
    if (!text) return;
    line.push((u.prefix && line.length === 0 ? u.prefix : "") + text);
  });
  endLine();
  return out.join("\n").replace(/\n{3,}/g, "\n\n").trim() + "\n";
}

async function req<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  if (!res.ok) {
    let detail = `${res.status} ${res.statusText}`;
    try { const b = (await res.json()) as { error?: string }; if (b.error) detail = b.error; } catch { /* keep the status */ }
    throw new Error(detail);
  }
  return (await res.json()) as T;
}

export const rewriteApi = {
  note: (before: string, sentence: string, after: string) => req<Note>("/api/rewrite/note", { before, sentence, after }),
  fetch: (url: string) => req<{ title: string; text: string }>("/api/rewrite/fetch", { url }),
};

type Doc = { title: string; text: string; versions: Record<number, Version>; cur: number };
const KEY = "bough.rewrite";

function loadDoc(): Doc | null {
  try { const raw = localStorage.getItem(KEY); return raw ? (JSON.parse(raw) as Doc) : null; } catch { return null; }
}
function saveDoc(d: Doc | null) {
  try { if (d) localStorage.setItem(KEY, JSON.stringify(d)); else localStorage.removeItem(KEY); } catch { /* private window */ }
}

const isNotion = (s: string) => /^https?:\/\/(www\.)?([a-z0-9-]+\.)?notion\.(so|site)\/\S+$/i.test(s.trim());

/** The tells marked inside the sentence. */
function Marked({ text, tells }: { text: string; tells: string[] }) {
  const parts: { s: string; tell: boolean }[] = [{ s: text, tell: false }];
  for (const t of tells) {
    if (!t) continue;
    for (let i = 0; i < parts.length; i++) {
      const p = parts[i];
      if (p.tell) continue;
      const at = p.s.indexOf(t);
      if (at < 0) continue;
      parts.splice(i, 1, { s: p.s.slice(0, at), tell: false }, { s: t, tell: true }, { s: p.s.slice(at + t.length), tell: false });
      i += 2;
    }
  }
  return <>{parts.filter((p) => p.s).map((p, i) => p.tell ? <mark key={i} className="rw-tell">{p.s}</mark> : <span key={i}>{p.s}</span>)}</>;
}

function Start({ onStart }: { onStart: (title: string, text: string) => void }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const link = isNotion(text);
  const go = async () => {
    if (!text.trim() || busy) return;
    setErr("");
    if (!link) { onStart("Pasted document", text); return; }
    setBusy(true);
    try { const got = await rewriteApi.fetch(text.trim()); onStart(got.title || "Notion page", got.text); }
    catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  return (
    <div className="rw-start">
      <h2 className="rw-start-h">Rewrite a document in your own words</h2>
      <p className="rw-start-p">Paste the AI draft, or a Notion page link. You get it back one sentence at a time, with what each one claims and where it reads like a machine. You type every word of the result.</p>
      <textarea className="field rw-start-box" value={text} aria-label="Document or Notion link" placeholder="Paste the draft here, or a notion.so link…"
                onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); void go(); } }} />
      <div className="rw-start-acts">
        <button type="button" className="btn btn-primary" disabled={!text.trim() || busy} onClick={() => { void go(); }}>{busy ? "Reading…" : link ? "Read the page" : "Start"}</button>
        <span className="rw-dim">{link ? "Notion link: read through the NOTION_TOKEN in ~/.bough/env." : "⌘↩ to start"}</span>
      </div>
      {err && <p className="err rw-err" role="alert">{err}</p>}
    </div>
  );
}

export function RewritePage({ onBack }: { onBack?: () => void }) {
  const [doc, setDoc] = useState<Doc | null>(() => loadDoc());
  const units = useMemo(() => (doc ? split(doc.text) : []), [doc?.text]);
  const textIdx = useMemo(() => units.map((u, i) => (u.kind === "text" ? i : -1)).filter((i) => i >= 0), [units]);
  const [notes, setNotes] = useState<Record<number, Note | { error: string }>>({});
  const [showDoc, setShowDoc] = useState(false);
  const [draft, setDraft] = useState("");
  const [copied, copy] = useCopied();
  const box = useRef<HTMLTextAreaElement>(null);
  const asked = useRef(new Set<number>());

  useEffect(() => saveDoc(doc), [doc]);

  const cur = doc?.cur ?? -1;
  const pos = textIdx.indexOf(cur);
  const done = doc ? textIdx.filter((i) => doc.versions[i]).length : 0;

  // The box carries the draft of the sentence you are on; moving loads
  // that sentence's version, so going back shows what you wrote.
  useEffect(() => {
    if (!doc || cur < 0) return;
    const v = doc.versions[cur];
    setDraft(v && v.action === "rewrite" ? v.text : "");
    box.current?.focus();
  }, [cur, doc?.text]);

  const ask = useCallback((i: number) => {
    if (i < 0 || asked.current.has(i) || units[i]?.kind !== "text") return;
    asked.current.add(i);
    const at = textIdx.indexOf(i);
    const before = at > 0 ? units[textIdx[at - 1]].text : "";
    const after = at >= 0 && at + 1 < textIdx.length ? units[textIdx[at + 1]].text : "";
    rewriteApi.note(before, units[i].text, after)
      .then((n) => setNotes((m) => ({ ...m, [i]: n })))
      .catch((e) => { asked.current.delete(i); setNotes((m) => ({ ...m, [i]: { error: e instanceof Error ? e.message : String(e) } })); });
  }, [units, textIdx]);

  // The note for this sentence, and the next one's ahead of time.
  useEffect(() => {
    if (cur < 0) return;
    ask(cur);
    if (pos >= 0 && pos + 1 < textIdx.length) ask(textIdx[pos + 1]);
  }, [cur, pos, ask, textIdx]);

  const start = (title: string, text: string) => {
    const us = split(text);
    const first = us.findIndex((u) => u.kind === "text");
    asked.current = new Set();
    setNotes({});
    setDoc({ title, text, versions: {}, cur: first });
    setShowDoc(false);
  };
  const clear = () => { setDoc(null); setNotes({}); asked.current = new Set(); };

  const commit = (action: Version["action"], text = draft) => {
    if (!doc || cur < 0) return;
    if (action === "rewrite" && !text.trim()) return;
    // After the last sentence the cursor parks: the done screen shows
    // until a row in the doc view opens a sentence again.
    const next = pos + 1 < textIdx.length ? textIdx[pos + 1] : -1;
    setDoc({ ...doc, versions: { ...doc.versions, [cur]: { text, action } }, cur: next });
  };
  const goTo = (i: number) => { if (doc) setDoc({ ...doc, cur: i }); setShowDoc(false); };
  const move = (d: number) => { if (pos + d >= 0 && pos + d < textIdx.length) goTo(textIdx[pos + d]); };

  const onKey = (e: React.KeyboardEvent) => {
    const mod = e.metaKey || e.ctrlKey;
    if (!mod) return;
    if (e.key === "Enter") { e.preventDefault(); commit("rewrite"); }
    else if (e.key.toLowerCase() === "k") { e.preventDefault(); commit("keep"); }
    else if (e.key === "Backspace") { e.preventDefault(); commit("cut"); }
    else if (e.key === "ArrowUp") { e.preventDefault(); move(-1); }
    else if (e.key === "ArrowDown") { e.preventDefault(); move(1); }
  };

  if (!doc) return <div className="rw"><section className="rw-main"><header className="rw-bar"><Back onBack={onBack} /><h1 className="rw-title">Rewrite</h1></header><Start onStart={start} /></section></div>;

  const result = assemble(units, doc.versions);
  const finished = cur < 0;
  const u = units[cur];
  const note = notes[cur];
  const before = pos > 0 ? units[textIdx[pos - 1]] : null;
  const after = pos + 1 < textIdx.length ? units[textIdx[pos + 1]] : null;
  const counts = Object.values(doc.versions).reduce((c, v) => { c[v.action]++; return c; }, { rewrite: 0, keep: 0, cut: 0 });

  return (
    <div className="rw">
      <section className="rw-main">
        <header className="rw-bar">
          <Back onBack={onBack} />
          <h1 className="rw-title">{doc.title}</h1>
          <span className="rw-prog num" aria-label={`${done} of ${textIdx.length} sentences`}>
            {done} / {textIdx.length}
            <span className="rw-track" aria-hidden="true"><i style={{ width: `${textIdx.length ? (100 * done) / textIdx.length : 0}%` }} /></span>
          </span>
          <button type="button" className={"btn btn-sm" + (showDoc ? " is-on" : "")} aria-pressed={showDoc} onClick={() => setShowDoc((s) => !s)}>{showDoc ? "Hide doc" : "Show doc"}</button>
          <button type="button" className="btn btn-sm btn-primary" onClick={() => copy(result)}>{copied ? "Copied" : "Copy result"}</button>
        </header>

        {showDoc ? (
          <div className="scroll rw-doc" role="list" aria-label="Every sentence">
            <div className="rw-doc-hd"><span className="eyebrow">AI wrote</span><span className="eyebrow">You write</span></div>
            {units.map((un, i) => un.kind === "struct"
              ? <div key={i} className="rw-doc-struct mono" role="listitem">{un.text}</div>
              : (
                <button key={i} type="button" role="listitem" className={"rw-doc-row" + (i === cur ? " is-cur" : "") + (doc.versions[i] ? " is-done" : "")} onClick={() => goTo(i)}>
                  <span className="rw-doc-l">{un.prefix ? <span className="rw-dim">{un.prefix.trim()} </span> : null}{un.text}</span>
                  <span className={"rw-doc-r" + (doc.versions[i]?.action === "cut" ? " is-cut" : doc.versions[i]?.action === "keep" ? " is-kept" : "")}>
                    {!doc.versions[i] ? "" : doc.versions[i].action === "cut" ? "cut" : doc.versions[i].action === "keep" ? "kept as is" : doc.versions[i].text}
                  </span>
                </button>
              ))}
          </div>
        ) : finished ? (
          <div className="scroll rw-focus">
            <div className="rw-finished">
              <h2 className="rw-start-h">All {textIdx.length} sentences done</h2>
              <p className="rw-start-p">{counts.rewrite} rewritten, {counts.keep} kept, {counts.cut} cut. Copy the result, or open the doc to revisit a sentence.</p>
              <pre className="rw-result">{result}</pre>
              <div className="rw-start-acts">
                <button type="button" className="btn btn-primary" onClick={() => copy(result)}>{copied ? "Copied" : "Copy result"}</button>
                <button type="button" className="btn" onClick={() => setShowDoc(true)}>Show doc</button>
                <button type="button" className="btn" onClick={clear}>New document</button>
              </div>
            </div>
          </div>
        ) : (
          <div className="scroll rw-focus">
            <p className="rw-ctx" aria-hidden="true">
              {before && <span>{before.text} </span>}
              <span className="rw-ctx-cur">{u.text}</span>
              {after && <span> {after.text}</span>}
            </p>
            <div className="rw-two">
              <div className="rw-card" aria-label="The sentence the AI wrote">
                <span className="eyebrow">AI wrote · sentence {pos + 1}</span>
                <p className="rw-sent">{u.prefix ? <span className="rw-dim">{u.prefix.trim()} </span> : null}<Marked text={u.text} tells={note && "tells" in note ? note.tells : []} /></p>
                {!note && <p className="rw-note rw-dim">Reading the sentence…</p>}
                {note && "error" in note && <p className="rw-note err">{note.error} <button type="button" className="link" onClick={() => { asked.current.delete(cur); setNotes((m) => { const n = { ...m }; delete n[cur]; return n; }); ask(cur); }}>retry</button></p>}
                {note && "claim" in note && (
                  <dl className="rw-note">
                    {note.claim && <><dt>Claim</dt><dd>{note.claim}</dd></>}
                    {note.tells.length > 0 && <><dt>Tells</dt><dd>{note.tells.map((t, i) => <span key={i} className="rw-tell-chip">{t}</span>)}</dd></>}
                    {note.advice && <><dt>Do</dt><dd>{note.advice}</dd></>}
                  </dl>
                )}
                <div className="rw-acts">
                  <button type="button" className="btn btn-sm" onClick={() => commit("keep")}>Keep as is <kbd>⌘K</kbd></button>
                  <button type="button" className="btn btn-sm" onClick={() => commit("cut")}>Cut <kbd>⌘⌫</kbd></button>
                </div>
              </div>
              <div className="rw-card rw-card-you">
                <span className="eyebrow">You write</span>
                <textarea ref={box} className="field rw-box" value={draft} aria-label="Your version" placeholder="Say it your way…"
                          onChange={(e) => setDraft(e.target.value)} onKeyDown={onKey} />
                <div className="rw-acts">
                  <button type="button" className="btn btn-sm btn-primary" disabled={!draft.trim()} onClick={() => commit("rewrite")}>Next <kbd>⌘↩</kbd></button>
                  <span className="rw-dim">Enter adds a line, ⌘Enter moves on</span>
                </div>
              </div>
            </div>
          </div>
        )}

        <footer className="rw-foot">
          <button type="button" className="btn btn-sm" disabled={pos <= 0} onClick={() => move(-1)} aria-label="Previous sentence">← Prev</button>
          <button type="button" className="btn btn-sm" disabled={pos < 0 || pos + 1 >= textIdx.length} onClick={() => move(1)} aria-label="Next sentence without deciding">Skip →</button>
          <span className="rw-dim num">{counts.rewrite} rewritten · {counts.keep} kept · {counts.cut} cut</span>
          <span className="rw-sp" />
          <button type="button" className="link rw-dim" onClick={clear}>New document</button>
        </footer>
      </section>
    </div>
  );
}
