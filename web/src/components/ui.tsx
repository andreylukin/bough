// Small shared primitives used across the surfaces.
import { useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { c, alpha, mono } from "../theme";

async function copyText(s: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(s);
  } catch {
    // The desktop webview / non-secure contexts may lack the async clipboard API.
    const ta = document.createElement("textarea");
    ta.value = s;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}

// Click-to-copy chip for ids (session, session/turn). Flashes ✓ so you know it landed;
// the tooltip shows the exact value that will be copied.
export function CopyId({ value, label = "id", title }: { value: string; label?: string; title: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      onClick={(e) => {
        e.stopPropagation();
        copyText(value).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        });
      }}
      title={`${title}\n${value}`}
      style={{
        fontFamily: mono,
        fontSize: 10.5,
        letterSpacing: 0,
        fontWeight: 400,
        color: copied ? c.green : c.muted2,
        border: `1px solid ${copied ? c.green : c.border2}`,
        borderRadius: 5,
        padding: "1px 7px",
        lineHeight: 1.5,
        flex: "none",
      }}
    >
      {copied ? "✓ copied" : `⧉ ${label}`}
    </button>
  );
}

// The bough mark — a rotated rounded square with an inner fill.
export function Logo({ size = 16, filled = false }: { size?: number; filled?: boolean }) {
  return (
    <div
      style={{
        width: size,
        height: size,
        borderRadius: size * 0.28,
        border: `1.5px solid ${c.green}`,
        transform: "rotate(45deg)",
        position: "relative",
        flex: "none",
      }}
    >
      {filled && (
        <span
          style={{
            position: "absolute",
            inset: size * 0.22,
            background: c.green,
            borderRadius: 2,
            opacity: 0.85,
          }}
        />
      )}
    </div>
  );
}

// A live status dot with the green glow.
export function Dot({ color = c.green, pulse = false }: { color?: string; pulse?: boolean }) {
  const amber = color === c.amber;
  return (
    <span
      className={pulse ? (amber ? "pulse-amber" : "pulse-green") : undefined}
      style={{
        width: 7,
        height: 7,
        borderRadius: "50%",
        background: color,
        flex: "none",
        boxShadow: pulse
          ? undefined
          : `0 0 0 3px ${amber ? alpha(c.amber, 16) : alpha(c.green, 18)}`,
      }}
    />
  );
}

// The amber annotation badge used throughout the design brief. Kept for parity with
// the brief but off by default in the live app (annotations are a design-doc device).
export function Badge({ children }: { children: ReactNode }) {
  return (
    <span
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        width: 17,
        height: 17,
        borderRadius: "50%",
        background: c.amber,
        color: c.panel,
        fontSize: 10,
        fontWeight: 600,
        fontFamily: mono,
        flex: "none",
      }}
    >
      {children}
    </span>
  );
}

// A monospace count chip (e.g. tab badges).
export function Chip({ children, style }: { children: ReactNode; style?: CSSProperties }) {
  return (
    <span
      style={{
        fontFamily: mono,
        fontSize: 10,
        background: c.border2,
        borderRadius: 4,
        padding: "1px 5px",
        color: c.muted,
        ...style,
      }}
    >
      {children}
    </span>
  );
}

export const Kbd = ({ children }: { children: ReactNode }) => (
  <kbd
    style={{
      background: c.border2,
      border: `1px solid ${c.hairline}`,
      borderRadius: 4,
      padding: "1px 6px",
      color: c.muted,
      fontFamily: mono,
      fontSize: 11,
    }}
  >
    {children}
  </kbd>
);
