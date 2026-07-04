// Markdown for agent replies via react-markdown + remark-gfm — headings, lists,
// tables, code, links. No HTML passthrough: agent text is untrusted, and
// react-markdown renders everything as React nodes (raw HTML is dropped, URLs
// sanitized). While a turn streams, remend repairs unterminated constructs
// (dangling **bold**, open fences) so partial markdown doesn't flash as syntax.
// Styled to the bough look: quiet type shifts, mono code on a panel inset.
import React from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remend from "remend";
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

const cellBase: React.CSSProperties = {
  padding: "5px 10px",
  textAlign: "left",
  verticalAlign: "top",
};

// Distinguishes fenced blocks (code inside pre) from inline `code`.
const InPre = React.createContext(false);

function heading(size: number) {
  return ({ children }: { children?: React.ReactNode }) => (
    <div style={{ fontWeight: 600, color: c.text, fontSize: size, margin: "14px 0 4px" }}>{children}</div>
  );
}

const components: Components = {
  h1: heading(15.5),
  h2: heading(15.5),
  h3: heading(14.5),
  h4: heading(14.5),
  h5: heading(14.5),
  h6: heading(14.5),
  p: ({ children }) => <div style={{ margin: "8px 0" }}>{children}</div>,
  a: ({ href, children }) => (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      style={{ color: c.green, textDecoration: "none", borderBottom: `1px solid ${c.border}` }}
    >
      {children}
    </a>
  ),
  strong: ({ children }) => <strong style={{ color: c.text, fontWeight: 600 }}>{children}</strong>,
  ul: ({ children }) => <ul className="md-list" style={{ margin: "4px 0", paddingLeft: 22 }}>{children}</ul>,
  ol: ({ children }) => <ol className="md-list" style={{ margin: "4px 0", paddingLeft: 22 }}>{children}</ol>,
  li: ({ children }) => <li style={{ padding: "1px 0" }}>{children}</li>,
  blockquote: ({ children }) => (
    <blockquote style={{ margin: "8px 0", paddingLeft: 12, borderLeft: `2px solid ${c.border}`, color: c.muted }}>
      {children}
    </blockquote>
  ),
  hr: () => <hr style={{ border: "none", borderTop: `1px solid ${c.border2}`, margin: "12px 0" }} />,
  pre: ({ children }) => (
    <pre style={codeBlock}>
      <InPre.Provider value={true}>{children}</InPre.Provider>
    </pre>
  ),
  code: function MdCode({ children }) {
    const inPre = React.useContext(InPre);
    return inPre ? <code>{children}</code> : <code style={inlineCode}>{children}</code>;
  },
  table: ({ children }) => (
    <div style={{ overflowX: "auto", margin: "8px 0" }}>
      <table style={{ borderCollapse: "collapse", fontSize: 13.5, lineHeight: 1.5 }}>{children}</table>
    </div>
  ),
  th: ({ children, style }) => (
    <th style={{ ...cellBase, ...style, color: c.text, fontWeight: 600, borderBottom: `1px solid ${c.border}` }}>
      {children}
    </th>
  ),
  td: ({ children, style }) => (
    <td style={{ ...cellBase, ...style, color: c.text2, borderBottom: `1px solid ${c.border3}` }}>{children}</td>
  ),
  // GFM task-list checkboxes
  input: ({ checked }) => (
    <input type="checkbox" checked={!!checked} readOnly style={{ accentColor: c.green, marginRight: 6 }} />
  ),
};

export const Markdown = React.memo(function Markdown({ text, streaming }: { text: string; streaming?: boolean }) {
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
      {streaming ? remend(text) : text}
    </ReactMarkdown>
  );
});
