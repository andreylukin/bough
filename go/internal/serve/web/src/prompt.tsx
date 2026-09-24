import { Fragment, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { anchorPlace } from "./popover";

// A big paste is sent wrapped, so the transcript can fold it back into
// the chip it was in the composer. The model reads the tag as what it is.
// A private-use sentinel stands in for it while the rest is parsed, so a
// paste that itself holds "\n[file: x]\n" never splits the prompt.
const PASTE = /<pasted-text lines="(\d+)">\n([\s\S]*?)\n<\/pasted-text>/g;
const MARK = /(\d+)/g;

export function wrapPaste(text: string, lines: number) {
  return `<pasted-text lines="${lines}">\n${text}\n</pasted-text>`;
}

/** The draft's "[Image #N]", "[File #N]" and "[Pasted text #N +L lines]" tags whose content is gone (a failed upload, an empty slot): sending one would send the placeholder. */
export function lostTags(text: string, images: string[], pastes: string[]) {
  return [...text.matchAll(/\[(?:Image|File) #(\d+)\]|\[Pasted text #(\d+) \+\d+ lines\]/g)]
    .filter((m) => m[1] ? !images[+m[1] - 1] : pastes[+m[2] - 1] === undefined)
    .map((m) => m[0]);
}

/** A sent prompt back into the composer: each wrapped paste becomes its tag again, its text handed to `keep`, which returns the tag's number. */
export function foldPastes(text: string, keep: (body: string) => number) {
  return text.replace(PASTE, (_, n, body) => `[Pasted text #${keep(body)} +${n} lines]`);
}

export type Att = { kind: "skill" | "file" | "paste"; label: string; body: string; /** Where its chip goes in `said`, or -1 for the strip below. */ at: number; len: number };

/** A recorded prompt, split into what you typed and what rode along. */
export function parsePrompt(text: string) {
  const pastes: { lines: string; body: string }[] = [];
  const marked = text.replace(PASTE, (_, lines, body) => `${pastes.push({ lines, body }) - 1}`);
  // The loop appends "[skill: name]\n<SKILL.md>" blocks to the prompt a
  // skill was invoked from. What you typed is the part before them.
  // An @file is attached the same way, as "[file: path]\n<contents>":
  // pasted source is context, not the words of the prompt.
  const [typed, ...blocks] = marked.split(/\n+(?=\[(?:skill|file): [^\]\n]+\]\n)/);
  const unmark = (s: string) => s.replace(MARK, (_, i) => wrapPaste(pastes[+i].body, +pastes[+i].lines));
  const raw = unmark(typed);
  // A pasted image rides as "[Image #N: path]": show the tag and the picture, not the path.
  const images = [...typed.matchAll(/\[Image #\d+: ([^\]\n]+)\]/g)].map((m) => m[1]);
  const said = typed.replace(/\[Image (#\d+): [^\]\n]+\]/g, "[Image $1]");
  const atts: Att[] = [];
  for (const m of said.matchAll(MARK)) {
    const p = pastes[+m[1]];
    atts.push({ kind: "paste", label: `Pasted ${p.lines} line${p.lines === "1" ? "" : "s"}`, body: p.body, at: m.index!, len: m[0].length });
  }
  for (const s of blocks) {
    const [head, ...body] = s.split("\n");
    const m = /^\[(skill|file): (.+)\]$/.exec(head.trim());
    const kind = m?.[1] === "file" ? "file" : "skill";
    const label = (kind === "file" ? "@" : "/") + (m?.[2] ?? head);
    // Named once: as a chip where the prompt mentions it (/exa,
    // @go/serve.go), else in the strip below.
    let at = -1;
    for (let i = said.indexOf(label); i >= 0; i = said.indexOf(label, i + 1)) {
      if ((i === 0 || /\s/.test(said[i - 1])) && !atts.some((a) => i < a.at + a.len && a.at < i + label.length)) { at = i; break; }
    }
    atts.push({ kind, label, body: unmark(body.join("\n")), at, len: label.length });
  }
  // What the prompt reads as with its chips folded: what the clamp measures.
  const plain = said.replace(MARK, (_, i) => `[Pasted ${pastes[+i].lines} lines]`);
  return { raw, said, plain, images, atts };
}

/** The typed words with each attachment as a chip in place; the rest go in `loose`. */
export function promptNodes(said: string, atts: Att[]) {
  const nodes: React.ReactNode[] = [];
  const placed = new Set<number>();
  let pos = 0;
  for (const i of atts.map((_, i) => i).filter((i) => atts[i].at >= 0).sort((a, b) => atts[a].at - atts[b].at)) {
    if (atts[i].at < pos) continue;
    nodes.push(said.slice(pos, atts[i].at), <AttChip key={"att" + i} att={atts[i]} />);
    placed.add(i);
    pos = atts[i].at + atts[i].len;
  }
  nodes.push(said.slice(pos));
  return { nodes, loose: atts.filter((_, i) => !placed.has(i)) };
}

/** A user message's words, attachments folded to chips: the steer bubble and the pending bubble. */
export function PromptWords({ text }: { text: string }) {
  const { said, atts } = parsePrompt(text);
  const { nodes, loose } = promptNodes(said, atts);
  return <>{nodes}{loose.map((a, i) => <Fragment key={"loose" + i}>{" "}<AttChip att={a} /></Fragment>)}</>;
}

const TITLE = { skill: "Skill", file: "Attached file", paste: "Pasted text" } as const;

/**
 * An attachment as a chip. Its contents show on hover or focus, and a
 * click pins them open until clicked again or Esc: a paste or a SKILL.md
 * inline would bury the words you typed around it.
 */
export function AttChip({ att }: { att: Att }) {
  const [hover, setHover] = useState(false);
  const [pinned, setPinned] = useState(false);
  const chip = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const hide = useRef<ReturnType<typeof setTimeout>>(undefined);
  const id = useId();
  const open = hover || pinned;
  // Leaving the chip for the popover crosses a gap; a short grace keeps it open.
  const enter = () => { clearTimeout(hide.current); setHover(true); };
  const leave = () => { clearTimeout(hide.current); hide.current = setTimeout(() => setHover(false), 150); };
  useEffect(() => () => clearTimeout(hide.current), []);
  // Fixed and portalled: a clamped prompt hides its overflow, and a
  // transcript scroll moves the chip, so the popover follows it.
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const c = chip.current, p = pop.current;
      if (!c || !p) return;
      const r = c.getBoundingClientRect();
      const vw = document.documentElement.clientWidth, vh = window.innerHeight;
      const at = anchorPlace(r, p.offsetWidth, p.scrollHeight, { left: 0, right: vw, top: 0, bottom: vh }, "end");
      Object.assign(p.style, { left: at.left + "px", top: at.top + "px", maxHeight: Math.min(at.maxHeight, 360) + "px" });
    };
    place();
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => { window.removeEventListener("scroll", place, true); window.removeEventListener("resize", place); };
  }, [open]);
  useEffect(() => {
    if (!pinned) return;
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); setPinned(false); setHover(false); chip.current?.focus(); } };
    const away = (e: MouseEvent) => { if (!chip.current?.contains(e.target as Node) && !pop.current?.contains(e.target as Node)) setPinned(false); };
    document.addEventListener("keydown", esc, true);
    document.addEventListener("mousedown", away);
    return () => { document.removeEventListener("keydown", esc, true); document.removeEventListener("mousedown", away); };
  }, [pinned]);
  return (
    <>
      <button ref={chip} type="button" className={"mono prompt-chip prompt-chip-" + att.kind} title={pinned ? undefined : TITLE[att.kind]}
              aria-expanded={pinned} aria-controls={open ? id : undefined}
              onMouseEnter={enter} onMouseLeave={leave}
              onFocus={enter} onBlur={(e) => { if (!pop.current?.contains(e.relatedTarget as Node)) leave(); }}
              onClick={() => setPinned((v) => !v)}>
        {att.label}
      </button>
      {open && typeof document !== "undefined" && createPortal(
        <div ref={pop} id={id} role="region" aria-label={`${TITLE[att.kind]} ${att.label}`} tabIndex={pinned ? 0 : -1}
             className={"prompt-pop" + (att.kind === "file" ? " prompt-att-file" : "") + (pinned ? " pinned" : "")}
             onMouseEnter={enter} onMouseLeave={leave}
             onBlur={(e) => { if (e.relatedTarget !== chip.current && !pop.current?.contains(e.relatedTarget as Node)) leave(); }}>
          <pre className="mono">{att.body}</pre>
        </div>,
        document.body)}
    </>
  );
}
