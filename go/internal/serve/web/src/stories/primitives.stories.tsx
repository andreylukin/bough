import type { Meta, StoryObj } from "@storybook/react-vite";

// The stylesheet's small parts, as class markup: what a new screen is
// built from. Colour is by token, never by hand.
const meta: Meta = { title: "Foundations", parameters: { layout: "padded" } };
export default meta;

const tokens: Array<[string, string, string]> = [
  ["--bg", "#0f1310", "page"], ["--surface", "#191d1a", "sidebar, blocks"], ["--raised", "#222723", "fields, cards"],
  ["--sel", "#242b25", "selected row"], ["--line", "#343a35", "decorative rule"], ["--line-strong", "#646b65", "control border"],
  ["--text-1", "#e3eae4", "primary text"], ["--text-2", "#b1b8b2", "secondary"], ["--text-3", "#89908a", "labels, hints"],
  ["--accent", "#82cb9b", "interactive"], ["--amber", "#eabc6e", "waiting for you"], ["--red", "#ed756e", "failed"],
];

export const Colors: StoryObj = {
  render: () => (
    <div style={{ display: "grid", gridTemplateColumns: "repeat(4, minmax(0, 1fr))", gap: 12, maxWidth: 720 }}>
      {tokens.map(([name, hex, use]) => (
        <div key={name} style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <i style={{ display: "block", height: 44, borderRadius: 7, border: "1px solid var(--line)", background: `var(${name})` }} />
          <b style={{ fontSize: 13, fontWeight: 500 }}>{name}</b>
          <span className="mono" style={{ fontSize: 12, color: "var(--text-3)" }}>{hex} · {use}</span>
        </div>
      ))}
    </div>
  ),
};

export const Buttons: StoryObj = {
  render: () => (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <div style={{ display: "flex", gap: 10 }}><button className="btn">Rename</button><button className="btn">Archive</button><button className="btn">Stop</button></div>
      <div style={{ display: "flex", gap: 10 }}><button className="btn btn-primary">Send</button><button className="btn btn-primary">New project</button></div>
      <div style={{ display: "flex", gap: 10 }}><button className="btn btn-primary" disabled>Send</button><button className="btn" disabled>Stop</button></div>
      <div style={{ display: "flex", gap: 10 }}><button className="link">Show archived</button><button className="link">Rename</button></div>
    </div>
  ),
};

export const Fields: StoryObj = {
  render: () => (
    <div style={{ display: "flex", flexDirection: "column", gap: 20, maxWidth: 420 }}>
      <div style={{ width: 264 }}>
        <label htmlFor="q" className="field-label">Search sessions</label>
        <input id="q" className="field" placeholder="Title, repo or branch" />
      </div>
      <label className="ctl"><span className="ctl-label">Model</span>
        <select><option>claude-sonnet-5 — 200k</option><option>as configured</option></select></label>
    </div>
  ),
};

export const Feedback: StoryObj = {
  render: () => (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <div className="err">provider: 429 rate limited, retrying in 8s</div>
      <div className="meta-line">usage · 12.4k in, 1.1k out</div>
      <div className="toast" role="status" style={{ position: "static" }}>GET /api/sessions: 502 Bad Gateway</div>
    </div>
  ),
};
