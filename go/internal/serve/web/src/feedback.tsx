import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useModal } from "./dialog";

const issueURL = "https://github.com/andreylukin/bough/issues/new";

export function feedbackURL(title: string, details: string): string {
  const url = new URL(issueURL);
  url.searchParams.set("title", title.trim());
  url.searchParams.set("body", `## What happened\n${details.trim()}\n\n## Environment\nbough web`);
  return url.toString();
}

export function FeedbackDialog({ onClose }: { onClose: () => void }) {
  const [title, setTitle] = useState("");
  const [details, setDetails] = useState("");
  const [shot, setShot] = useState<File | null>(null);
  const [preview, setPreview] = useState("");
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState("");
  const box = useRef<HTMLDivElement>(null);
  const titleInput = useRef<HTMLInputElement>(null);
  const opener = useRef(document.activeElement as HTMLElement | null);
  useModal(box, true);

  useEffect(() => { titleInput.current?.focus(); return () => { const el = opener.current; setTimeout(() => el?.isConnected && el.focus(), 0); }; }, []);
  useEffect(() => {
    if (!shot) { setPreview(""); return; }
    const url = URL.createObjectURL(shot);
    setPreview(url);
    return () => URL.revokeObjectURL(url);
  }, [shot]);

  const copy = async () => {
    if (!shot) return;
    try {
      await navigator.clipboard.write([new ClipboardItem({ "image/png": shot })]);
      setCopied(true); setError("");
    } catch (e) {
      setError(`Could not copy the screenshot (${e instanceof Error ? e.message : String(e)}). Attach the file directly in GitHub.`);
    }
  };

  return createPortal(<div className="dlg-scrim" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
    <div ref={box} className="dlg feedback-dialog" role="dialog" aria-modal="true" aria-labelledby="feedback-title"
         onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); onClose(); } }}>
      <h2 id="feedback-title" className="dlg-title">Send feedback</h2>
      <p className="dlg-body">Review what you share. This opens a public GitHub issue; nothing is sent from this page.</p>
      <label>Issue title<input ref={titleInput} className="field dlg-input" value={title} onChange={(e) => setTitle(e.target.value)} /></label>
      <label>What happened<textarea className="field dlg-input" rows={5} value={details} onChange={(e) => setDetails(e.target.value)} placeholder="Steps to reproduce, what happened, and what you expected" /></label>
      <label>Screenshot (optional, PNG)<input type="file" accept="image/png" onChange={(e) => { setShot(e.target.files?.[0] ?? null); setCopied(false); setError(""); }} /></label>
      {preview && <img className="feedback-preview" src={preview} alt="Screenshot selected for review" />}
      {shot && <p className="dlg-body">Check the image for private content before sharing. {copied ? "Copied. Paste it into the GitHub issue after it opens." : "Copy it, then paste it into the GitHub issue."}</p>}
      {error && <p className="dlg-err" role="alert">{error}</p>}
      <div className="dlg-actions">
        <button type="button" className="btn" onClick={onClose}>Cancel</button>
        {shot && <button type="button" className="btn" onClick={copy}>Copy screenshot</button>}
        <a className="btn btn-primary" href={title.trim() && details.trim() ? feedbackURL(title, details) : undefined}
           target="_blank" rel="noopener noreferrer" aria-disabled={!title.trim() || !details.trim() || undefined}>Open GitHub issue</a>
      </div>
    </div>
  </div>, document.body);
}
