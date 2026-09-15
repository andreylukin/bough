import type { Meta, StoryObj } from "@storybook/react-vite";
import { EmptyState, Pending } from "../loading";

// The stylesheet's small parts, as class markup: what a new screen is
// built from. Colour is by token, never by hand.
const meta: Meta = { title: "Foundations", parameters: { layout: "padded" } };
export default meta;

const tokens: Array<[string, string, string]> = [
  ["--bg", "#0f1310", "page"], ["--surface", "#191d1a", "sidebar, blocks"], ["--raised", "#222723", "fields, cards"],
  ["--sel", "#242b25", "selected row"], ["--line", "#343a35", "decorative rule"], ["--line-strong", "#6f766f", "control border"],
  ["--line-rail", "#3c483f", "turn log rail"], ["--hover", "text-1 6%", "row hover"],
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

const wide = { viewport: { defaultViewport: "desktop" }, chromatic: { viewports: [1440] } };
const phone = { viewport: { defaultViewport: "mobile1" }, chromatic: { viewports: [400] } };

export const Ghost: StoryObj = {
  render: () => (
    <div style={{ display: "flex", gap: 8 }}>
      <button className="btn btn-ghost">Cancel</button><button className="btn btn-ghost" disabled>Cancel</button>
      <kbd>⌘K</kbd><span className="eyebrow">Recent</span>
    </div>
  ),
};

const Rows = () => (
  <div style={{ display: "flex", flexDirection: "column", gap: 4, maxWidth: 1080 }}>
    {([["is-running", "Running", "bash", "go test ./internal/serve/...", "12s"],
       ["is-failed", "Failed", "job", "bun run typecheck", "1m"],
       ["is-waiting", "Waiting", "hook", "pre-push · deploy guard", "3s"],
       ["is-done", "Done", "subagent", "review the diff", "4m"]] as const).map(([cls, word, label, target, meta]) => (
      <div key={word} className="row-grid">
        <svg className={`state-mark ${cls}`} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-label={word}>
          <circle cx="12" cy="12" r="8.5" /></svg>
        <span>{label}</span>
        <span className="mono">{target}</span>
        <span className={`count ${cls}`}>{word === "Failed" ? 1 : ""}</span>
        <span className="num">{meta}</span>
      </div>
    ))}
  </div>
);
export const RowsWide: StoryObj = { render: Rows, parameters: wide };
export const RowsPhone: StoryObj = { render: Rows, parameters: phone };

const Empty = () => (
  <div style={{ display: "flex", flexDirection: "column", gap: 32 }}>
    <EmptyState title="No hooks yet" action={{ label: "Add a hook", onClick: () => {} }}>Hooks run before and after tools.</EmptyState>
    <Pending what="The page" err="wiki: not found" action={{ label: "Back to wiki", onClick: () => {} }} />
    <Pending what="The page" lines={6} />
  </div>
);
export const EmptyWide: StoryObj = { render: Empty, parameters: wide };
export const EmptyPhone: StoryObj = { render: Empty, parameters: phone };

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
