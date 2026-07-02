// Minimal markdown for agent replies — headings, bold, inline code, fenced code
// blocks, lists, links. Deliberately small (no dep, no HTML passthrough): agent
// text is untrusted, so everything renders as React text nodes. Tuned to the
// bough look: quiet type shifts, mono code on a panel inset, no extra color.
import React from "react";
import { c, mono } from "../theme";

const codeBlock: React.CSSProperties = {
  fontFamily: mono,
  fontSize: 12.5,
  lineHeight: 1.6,
  background: c.panelInset,
  border: `1px solid ${c.border3}`,
  borderRadius: 8,
  padding: "10px 12px",
  margin: "8px 0",
  overflowX: "auto",
  whiteSpace: "pre",
  color: c.text2,
};

const inlineCode: React.CSSProperties = {
  fontFamily: mono,
  fontSize: "0.92em",
  background: c.panelInset,
  border: `1px solid ${c.border3}`,
  borderRadius: 4,
  padding: "1px 5px",
};

/** Bold / `code` / [link](url) within one line. */
function renderInline(text: string, key: number): React.ReactNode {
  const out: React.ReactNode[] = [];
  // Tokenize: `code` first (protects its contents), then **bold**, then links.
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\[[^\]]+\]\((https?:\/\/[^\s)]+)\))/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1]) out.push(<code key={i++} style={inlineCode}>{m[1].slice(1, -1)}</code>);
    else if (m[2]) out.push(<strong key={i++} style={{ color: c.text, fontWeight: 600 }}>{m[2].slice(2, -2)}</strong>);
    else if (m[3] && m[4]) {
      const label = m[3].slice(1, m[3].indexOf("]"));
      out.push(
        <a key={i++} href={m[4]} target="_blank" rel="noreferrer" style={{ color: c.green, textDecoration: "none", borderBottom: `1px solid ${c.border}` }}>
          {label}
        </a>,
      );
    }
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return <React.Fragment key={key}>{out}</React.Fragment>;
}

/** Block-level pass: fences, headings, list items, paragraphs. */
export function Markdown({ text }: { text: string }) {
  const blocks: React.ReactNode[] = [];
  const lines = text.split("\n");
  let i = 0;
  let key = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trimStart().startsWith("```")) {
      // Fenced code: collect to the closing fence (or end — streaming may not
      // have delivered it yet; render what's there).
      const buf: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trimStart().startsWith("```")) buf.push(lines[i++]);
      i++; // skip closing fence if present
      blocks.push(<pre key={key++} style={codeBlock}>{buf.join("\n")}</pre>);
      continue;
    }
    const h = /^(#{1,4})\s+(.*)$/.exec(line);
    if (h) {
      const depth = h[1].length;
      blocks.push(
        <div
          key={key++}
          style={{
            fontWeight: 600,
            color: c.text,
            fontSize: depth <= 2 ? 15.5 : 14.5,
            margin: "14px 0 4px",
          }}
        >
          {renderInline(h[2], 0)}
        </div>,
      );
      i++;
      continue;
    }
    const li = /^(\s*)([-*]|\d+\.)\s+(.*)$/.exec(line);
    if (li) {
      blocks.push(
        <div key={key++} style={{ display: "flex", gap: 8, padding: "1px 0", marginLeft: li[1].length ? 16 : 0 }}>
          <span style={{ color: c.muted, flexShrink: 0 }}>{li[2] === "-" || li[2] === "*" ? "·" : li[2]}</span>
          <span>{renderInline(li[3], 0)}</span>
        </div>,
      );
      i++;
      continue;
    }
    if (line.trim() === "") {
      blocks.push(<div key={key++} style={{ height: 8 }} />);
      i++;
      continue;
    }
    blocks.push(<div key={key++}>{renderInline(line, 0)}</div>);
    i++;
  }
  return <>{blocks}</>;
}
