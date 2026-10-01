import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useModal } from "./dialog";

const issueURL = "https://github.com/andreylukin/bough/issues/new";
const maxScreenshotBytes = 10 * 1024 * 1024;

export function screenshotError(file: File): string {
  if (file.type !== "image/png") return "Use a PNG screenshot.";
  if (!file.size) return "The screenshot is empty. Choose or paste another PNG image.";
  if (file.size > maxScreenshotBytes) return "Use a screenshot of 10 MB or smaller.";
  return "";
}

export function pastedScreenshot(data: Pick<DataTransfer, "files" | "items">): File | null {
  // Browsers can expose the same file in both lists; take it only once.
  return Array.from(data.files).find((file) => file.type.startsWith("image/"))
    ?? Array.from(data.items).find((item) => item.kind === "file" && item.type.startsWith("image/"))?.getAsFile()
    ?? null;
}

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
  const [preview, setPreview] = useState<{ file: File; url: string } | null>(null);
  const [copied, setCopied] = useState(false);
  const [copying, setCopying] = useState(false);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState("");
  const currentShot = useRef<File | null>(null);
  const box = useRef<HTMLDivElement>(null);
  const titleInput = useRef<HTMLInputElement>(null);
  const shotInput = useRef<HTMLInputElement>(null);
  const opener = useRef(document.activeElement as HTMLElement | null);
  useModal(box, true);

  useEffect(() => { titleInput.current?.focus(); return () => { currentShot.current = null; const el = opener.current; setTimeout(() => el?.isConnected && el.focus(), 0); }; }, []);
  useEffect(() => {
    if (!shot) { setPreview(null); return; }
    const url = URL.createObjectURL(shot);
    setPreview({ file: shot, url });
    return () => URL.revokeObjectURL(url);
  }, [shot]);

  const selectShot = (file: File | null) => {
    const problem = file ? screenshotError(file) : "";
    if (problem) { setError(problem); return; }
    currentShot.current = file;
    setShot(file); setCopied(false); setCopying(false); setReady(false); setError("");
  };
  const copy = async () => {
    if (!shot || !ready || copying) return;
    setCopying(true); setCopied(false); setError("");
    try {
      await navigator.clipboard.write([new ClipboardItem({ "image/png": shot })]);
      // Replacing/removing the image or closing while permission is pending
      // must not mark a different screenshot as copied.
      if (currentShot.current !== shot) return;
      setCopied(true); setError("");
    } catch (e) {
      if (currentShot.current !== shot) return;
      setError(`Could not copy the screenshot (${e instanceof Error ? e.message : String(e)}). Download it, then attach the file in GitHub.`);
    } finally {
      if (currentShot.current === shot) setCopying(false);
    }
  };

  return createPortal(<div className="dlg-scrim" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
    <div ref={box} className="dlg feedback-dialog" role="dialog" aria-modal="true" aria-labelledby="feedback-title"
         onPaste={(e) => {
           const file = pastedScreenshot(e.clipboardData);
           if (!file) return; // Ordinary text still pastes into title/details.
           e.preventDefault(); e.stopPropagation(); selectShot(file);
         }}
         onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); onClose(); } }}>
      <h2 id="feedback-title" className="dlg-title">Send feedback</h2>
      <p className="dlg-body">Review what you share. This opens a public GitHub issue draft; nothing is posted until you submit it on GitHub.</p>
      <label>Issue title<input ref={titleInput} className="field dlg-input" value={title} onChange={(e) => setTitle(e.target.value)} /></label>
      <label>What happened<textarea className="field dlg-input" rows={5} value={details} onChange={(e) => setDetails(e.target.value)} placeholder="Steps to reproduce, what happened, and what you expected" /></label>
      <label>Screenshot (optional, PNG)<input ref={shotInput} type="file" accept="image/png" aria-describedby="feedback-paste-help" onChange={(e) => {
        const file = e.target.files?.[0];
        if (file) selectShot(file);
        e.target.value = ""; // Allow choosing the same file again after removal.
      }} /></label>
      <p id="feedback-paste-help" className="dlg-body">Paste a screenshot anywhere in this dialog with Ctrl+V or ⌘V, or choose a PNG file (up to 10 MB). A new image replaces the current one.</p>
      {preview && preview.file === shot && <img key={preview.url} className="feedback-preview" src={preview.url} alt="Screenshot selected for review"
        onLoad={() => { if (currentShot.current === shot) setReady(true); }}
        onError={() => { if (currentShot.current === shot) { selectShot(null); setError("Could not read the screenshot. Choose or paste another PNG image."); } }} />}
      {shot && <>
        <p className="dlg-body">Check the image for private content before sharing. {copied ? "Copied. Paste it into the GitHub issue after it opens." : "Copy it, then paste it into the GitHub issue, or download it and attach the file there."} Opening the issue does not attach the image. It stays here until you close this dialog.</p>
        <div className="dlg-actions">
          <button type="button" className="btn" onClick={() => { selectShot(null); shotInput.current?.focus(); }}>Remove screenshot</button>
          {ready && preview?.file === shot && <a className="btn" href={preview.url} download="bough-feedback.png">Download screenshot</a>}
          <button type="button" className="btn" disabled={!ready || copying} onClick={copy}>{copying ? "Copying…" : "Copy screenshot"}</button>
        </div>
      </>}
      {error && <p className="dlg-err" role="alert">{error}</p>}
      <div className="dlg-actions">
        <button type="button" className="btn" onClick={onClose}>Cancel</button>
        <a className="btn btn-primary" href={title.trim() && details.trim() ? feedbackURL(title, details) : undefined}
           target="_blank" rel="noopener noreferrer" aria-disabled={!title.trim() || !details.trim() || undefined}>Open GitHub issue</a>
      </div>
    </div>
  </div>, document.body);
}
