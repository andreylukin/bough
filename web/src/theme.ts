// Design tokens lifted from docs/design/bough-ui.html. One accent (green), amber
// reserved for pending/hold-and-ask, red for deny/danger. Everything else neutral-dark.
export const c = {
  bg: "#0e1013",
  panel: "#14161a",
  panel2: "#161a1f",
  panel3: "#191c21",
  panelInset: "#1f2329",
  canvas: "#111318",
  border: "#2b3038",
  border2: "#23272e",
  border3: "#1c2026",
  hairline: "#3a414c",
  text: "#e7e9ed",
  text2: "#c9cdd4",
  muted: "#9aa1ac",
  muted2: "#656c77",
  green: "#4ec98f",
  amber: "#d9b45f",
  red: "#e2776e",
  blue: "#5c88c9",
} as const;

export const mono = "'IBM Plex Mono', ui-monospace, monospace";
export const sans = "'IBM Plex Sans', system-ui, sans-serif";
