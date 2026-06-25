export const $ = (sel) => document.querySelector(sel);

export const el = (tag, cls, html) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (html != null) e.innerHTML = html;
  return e;
};

export const esc = (s) => (s || "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

export const clip = (s, n) => { s = (s || "").replace(/\s+/g, " ").trim(); return s.length > n ? s.slice(0, n) + "…" : s; };
// The harness appends `[full output saved: /Users/.../out_N.txt]` to a capped
// digest; the absolute path is internal noise, so show the truncation marker
// without it.

export const cleanDigest = (s) => (s || "").replace(/\[full output saved: [^\]]*\]/g, "[output truncated]");

export const ago = (ms) => {
  if (!ms) return "";
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return s + "s ago";
  const m = Math.floor(s / 60); if (m < 60) return m + "m ago";
  const h = Math.floor(m / 60); if (h < 24) return h + "h ago";
  const d = Math.floor(h / 24); return d + "d ago";
};

// ---- tiny markdown -> HTML (self-contained, no deps) ---------------------

export function mdInline(escaped) {
  return escaped
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*])\*([^*\n]+)\*/g, "$1<em>$2</em>")
    .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
}

export function md(src) {
  const lines = (src || "").split("\n");
  let html = "", i = 0, list = null;
  const closeList = () => { if (list) { html += `</${list}>`; list = null; } };
  while (i < lines.length) {
    const line = lines[i];
    const fence = line.match(/^```(\w*)\s*$/);
    if (fence) {
      closeList(); i++;
      const code = [];
      while (i < lines.length && !/^```\s*$/.test(lines[i])) { code.push(lines[i]); i++; }
      i++;
      html += `<details><summary>Code block</summary><pre class="code"><code>${esc(code.join("\n"))}</code></pre></details>`;
      continue;
    }
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) { closeList(); const l = Math.min(h[1].length + 2, 6); html += `<h${l} class="mdh">${mdInline(esc(h[2]))}</h${l}>`; i++; continue; }
    if (/^\s{0,3}>\s?/.test(line)) { closeList(); html += `<blockquote>${mdInline(esc(line.replace(/^\s{0,3}>\s?/, "")))}</blockquote>`; i++; continue; }
    if (/^\s*[-*+]\s+/.test(line)) {
      if (list !== "ul") { closeList(); html += "<ul>"; list = "ul"; }
      html += `<li>${mdInline(esc(line.replace(/^\s*[-*+]\s+/, "")))}</li>`; i++; continue;
    }
    if (/^\s*\d+\.\s+/.test(line)) {
      if (list !== "ol") { closeList(); html += "<ol>"; list = "ol"; }
      html += `<li>${mdInline(esc(line.replace(/^\s*\d+\.\s+/, "")))}</li>`; i++; continue;
    }
    if (line.trim() === "") { closeList(); i++; continue; }
    closeList();
    const para = [line]; i++;
    while (i < lines.length && lines[i].trim() !== "" &&
      !/^(```|#{1,6}\s|\s{0,3}>\s?|\s*[-*+]\s+|\s*\d+\.\s+)/.test(lines[i])) { para.push(lines[i]); i++; }
    html += `<p>${mdInline(esc(para.join("\n"))).replace(/\n/g, "<br>")}</p>`;
  }
  closeList();
  return html;
}

export function copyText(text, okMsg) {
  const done = () => toast(okMsg || "Copied");
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then(done).catch(() => fallbackCopy(text, done));
  } else {
    fallbackCopy(text, done);
  }
}

export function fallbackCopy(text, done) {
  const ta = document.createElement("textarea");
  ta.value = text; ta.style.position = "fixed"; ta.style.opacity = "0";
  document.body.appendChild(ta); ta.select();
  try { document.execCommand("copy"); done(); } catch { toast("Copy failed", true); }
  document.body.removeChild(ta);
}

export let toastTimer = null;

export function toast(msg, isErr) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = "show" + (isErr ? " err" : "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.className = ""), isErr ? 4500 : 2200);
}

export function humanTok(n) {
  if (n < 1000) return String(n);
  const k = n / 1000;
  return (k >= 100 ? Math.round(k) : Math.round(k * 10) / 10) + "k";
}

export const projBase = (p) => (p || "").split("/").filter(Boolean).pop() || p || "untitled";

export const fmtBytes = (n) => n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(1) + " KB" : (n / 1048576).toFixed(1) + " MB";

// The message actually sent: the typed text with any collapsed pastes appended.
