// Center pane when the Changes tab is focused (screen 4). Renders the selected file's
// hunks with jj/clonefile review affordances: applied hunks collapse to a quiet ✓,
// pending hunks offer Skip / Apply hunk; the header applies the whole file.
import { c, mono } from "../theme";
import type { DiffFile, DiffLine, Hunk } from "../mock";

const num = (n?: number) => (n === undefined ? "" : String(n));

function Line({ line }: { line: DiffLine }) {
  const bg =
    line.kind === "+" ? "rgba(78,201,143,.11)" : line.kind === "-" ? "rgba(226,119,110,.11)" : "transparent";
  const sign = line.kind === "+" ? c.green : line.kind === "-" ? c.red : "#3f4650";
  const textColor = line.kind === "+" ? "#9fd3b6" : line.kind === "-" ? "#d99a95" : "#8b929c";
  return (
    <div style={{ display: "flex", background: bg }}>
      <span style={{ width: 42, textAlign: "right", paddingRight: 10, color: "#3f4650" }}>{num(line.oldNo)}</span>
      <span style={{ width: 42, textAlign: "right", paddingRight: 10, color: "#4a7a5e" }}>{num(line.newNo)}</span>
      <span style={{ width: 20, color: sign }}>{line.kind === " " ? " " : line.kind}</span>
      <span style={{ color: textColor, whiteSpace: "pre" }}>{line.text}</span>
    </div>
  );
}

function HunkView({
  hunk,
  live,
  onApply,
  onSkip,
}: {
  hunk: Hunk;
  live: boolean;
  onApply: () => void;
  onSkip: () => void;
}) {
  const applied = hunk.status === "applied";
  return (
    <div>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "7px 16px 7px 20px",
          background: "#15181d",
          borderTop: `1px solid ${c.border2}`,
          borderBottom: `1px solid ${c.border2}`,
        }}
      >
        <span style={{ color: c.blue, fontSize: 11 }}>{hunk.header}</span>
        {applied ? (
          <span
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 6,
              fontSize: 10.5,
              color: c.green,
              padding: "3px 9px",
              borderRadius: 6,
              background: "rgba(78,201,143,.12)",
            }}
          >
            ✓ applied
          </span>
        ) : live ? (
          // Live apply is per-file (the backend has no per-hunk apply), so per-hunk
          // controls are hidden — the file header's "Apply file" is the live action.
          null
        ) : (
          <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
            <button onClick={onSkip} style={{ fontSize: 11, color: c.muted2, padding: "3px 10px", border: `1px solid ${c.border}`, borderRadius: 6 }}>
              Skip
            </button>
            <button
              onClick={onApply}
              style={{
                fontSize: 11,
                color: c.green,
                padding: "3px 11px",
                border: "1px solid rgba(78,201,143,.45)",
                borderRadius: 6,
              }}
            >
              Apply hunk
            </button>
          </span>
        )}
      </div>
      <div style={{ opacity: applied ? 0.6 : 1 }}>
        {hunk.lines.map((l, i) => (
          <Line key={i} line={l} />
        ))}
      </div>
    </div>
  );
}

export function DiffViewer({
  file,
  live = false,
  onApplyFile,
  onApplyHunk,
  onSkipHunk,
}: {
  file: DiffFile | null;
  live?: boolean;
  onApplyFile: () => void;
  onApplyHunk: (i: number) => void;
  onSkipHunk: (i: number) => void;
}) {
  return (
    <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", background: c.canvas }}>
      {file ? (
        <>
          <div
            style={{
              height: 46,
              flex: "none",
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              padding: "0 20px",
              borderBottom: `1px solid ${c.border2}`,
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <span
                style={{
                  fontFamily: mono,
                  fontSize: 10,
                  fontWeight: 600,
                  padding: "2px 7px",
                  borderRadius: 5,
                  background: "rgba(217,180,95,.15)",
                  color: c.amber,
                }}
              >
                {file.status}
              </span>
              <span style={{ fontFamily: mono, fontSize: 13, color: c.text }}>{file.path}</span>
              <span style={{ fontFamily: mono, fontSize: 11.5, color: c.green }}>+{file.added}</span>
              <span style={{ fontFamily: mono, fontSize: 11.5, color: c.red }}>−{file.removed}</span>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 9 }}>
              <span style={{ fontFamily: mono, fontSize: 11, color: c.muted2 }}>{file.meta}</span>
              <button
                onClick={onApplyFile}
                style={{ padding: "5px 12px", borderRadius: 7, background: c.green, color: c.bg, fontSize: 12, fontWeight: 600 }}
              >
                Apply file
              </button>
            </div>
          </div>

          <div style={{ flex: 1, overflowY: "auto", fontFamily: mono, fontSize: 12, lineHeight: 1.7 }}>
            {file.hunks.length > 0 ? (
              file.hunks.map((h, i) => (
                <HunkView key={i} hunk={h} live={live} onApply={() => onApplyHunk(i)} onSkip={() => onSkipHunk(i)} />
              ))
            ) : (
              <div style={{ padding: 24, color: c.muted2, fontFamily: mono, fontSize: 12 }}>
                No inline hunks staged for this file in the mock. Select auth/token.js to review a full diff.
              </div>
            )}
          </div>

          <div
            style={{
              height: 34,
              flex: "none",
              display: "flex",
              alignItems: "center",
              padding: "0 20px",
              borderTop: `1px solid ${c.border2}`,
              fontFamily: mono,
              fontSize: 10.5,
              color: c.muted2,
            }}
          >
            Applying writes from the snapshot into your working tree · reversible per file
          </div>
        </>
      ) : (
        <div style={{ padding: 40, color: c.muted2, fontSize: 14 }}>Select a file from the Changes rail to review its diff.</div>
      )}
    </div>
  );
}
