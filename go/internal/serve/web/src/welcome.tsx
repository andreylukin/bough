// The first thing someone sees on an empty server: connect a model, pick
// a folder the agent can edit, ask for something. It reads the same facts
// a session acts on (/api/setup), so the page cannot promise a writable
// folder the launcher then refuses. Before this, a first visit was an
// empty "Nothing needs your attention" and a New button that started a
// read-only session in home.
import { useEffect, useState } from "react";
import { api, type Setup, type SetupFolder, type SetupProvider } from "./api";

const DONE = "bough:welcome-done";

export function welcomeDismissed(): boolean {
  try { return localStorage.getItem(DONE) === "1"; } catch { return false; }
}

function dismiss() {
  try { localStorage.setItem(DONE, "1"); } catch { /* storage off */ }
}

const PROMPTS = [
  "Explain how this repo is organized",
  "Run the tests and fix anything that fails",
  "Find one small bug and fix it",
];

const LABEL: Record<string, string> = { anthropic: "Anthropic", openrouter: "OpenRouter", openai: "OpenAI", cerebras: "Cerebras" };

const tilde = (p: string, home: string) => (home && (p === home || p.startsWith(home + "/")) ? "~" + p.slice(home.length) : p);

export function Welcome({ onStart, onSkip }: { onStart: (cwd: string, prompt: string) => Promise<unknown> | void; onSkip: () => void }) {
  const [setup, setSetup] = useState<Setup | null>(null);
  const [err, setErr] = useState("");
  const [providers, setProviders] = useState<SetupProvider[]>([]);
  const [prov, setProv] = useState("anthropic");
  const [key, setKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [path, setPath] = useState("");
  const [folder, setFolder] = useState<SetupFolder | null>(null);
  const [prompt, setPrompt] = useState("");
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    api.setup().then((s) => {
      setSetup(s);
      setProviders(s.providers);
      setFolder(s.folder);
      setPath(tilde(s.folder.path, s.home));
    }).catch((e: Error) => setErr(e.message));
  }, []);

  // Each edit re-asks the server, so the line under the field is what a
  // session started there would actually get.
  useEffect(() => {
    if (!setup) return;
    const p = path.trim();
    if (!p) { setFolder(null); return; }
    let on = true;
    const t = setTimeout(() => { api.setup(p).then((s) => { if (on) setFolder(s.folder); }).catch(() => {}); }, 250);
    return () => { on = false; clearTimeout(t); };
  }, [path, setup]);

  const home = setup?.home ?? "";
  const keyed = providers.filter((p) => p.set);
  const ready = keyed.length > 0 && !!folder?.exists && !starting;

  const save = async () => {
    setSaving(true);
    setErr("");
    try {
      setProviders(await api.setKey(prov, key));
      setKey("");
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setSaving(false);
    }
  };

  const go = async (text: string) => {
    if (!ready || !folder || !text.trim()) return;
    setStarting(true);
    setErr("");
    try {
      dismiss();
      await onStart(folder.path, text.trim());
    } catch (e) {
      setErr((e as Error).message);
      setStarting(false);
    }
  };

  let status: { text: string; tone: "ok" | "warn" | "" };
  if (!path.trim()) status = { text: "Type the path of a project folder.", tone: "" };
  else if (!folder) status = { text: "Checking…", tone: "" };
  else if (!folder.exists) status = { text: "There is no folder at this path.", tone: "warn" };
  else if (folder.checkout) status = { text: `Git checkout ${tilde(folder.checkout, home)}: the agent can edit files here, and only here.`, tone: "ok" };
  else status = { text: "Not a git checkout: the agent can read files here but not edit them. Pick a repo to let it make changes.", tone: "warn" };

  return (
    <div className="welcome scroll">
      <div className="welcome-inner">
        <header className="welcome-head">
          <h1>Welcome to bough</h1>
          <p>A coding agent that works by writing one small program per step. Three steps to your first session.</p>
        </header>

        <ol className="welcome-steps">
          <li className={"welcome-step" + (keyed.length ? " done" : "")}>
            <h2><span className="welcome-num" aria-hidden="true">1</span>Connect a model</h2>
            {setup === null ? (
              <p className="welcome-note">{err ? "Could not check for keys." : "Checking for keys…"}</p>
            ) : keyed.length ? (
              <p className="welcome-status ok">Using {keyed.map((p) => p.env).join(", ")}. Switch models any time from a session.</p>
            ) : (<>
              <p className="welcome-note">
                No API key found. Paste one: it is saved to <code>{tilde(setup.envFile, home)}</code> on this machine and sent only to that provider.
              </p>
              <form className="welcome-row" onSubmit={(e) => { e.preventDefault(); void save(); }}>
                <select className="welcome-field" aria-label="Provider" value={prov} onChange={(e) => setProv(e.target.value)}>
                  {providers.map((p) => <option key={p.name} value={p.name}>{LABEL[p.name] ?? p.name}</option>)}
                </select>
                <input className="welcome-field grow" type="password" aria-label={`${LABEL[prov] ?? prov} API key`} placeholder="API key"
                  autoComplete="off" spellCheck={false} value={key} onChange={(e) => setKey(e.target.value)} />
                <button className="btn-primary" disabled={!key.trim() || saving}>{saving ? "Saving…" : "Save key"}</button>
              </form>
            </>)}
          </li>

          <li className={"welcome-step" + (folder?.checkout ? " done" : "")}>
            <h2><span className="welcome-num" aria-hidden="true">2</span>Pick a folder</h2>
            <input className="welcome-field wide" aria-label="Folder" placeholder="~/code/your-repo" spellCheck={false} autoComplete="off"
              value={path} onChange={(e) => setPath(e.target.value)} />
            <p className={"welcome-status " + status.tone} role="status">{status.text}</p>
          </li>

          <li className="welcome-step">
            <h2><span className="welcome-num" aria-hidden="true">3</span>Ask for something</h2>
            <div className="welcome-chips">
              {PROMPTS.map((p) => (
                <button key={p} className="welcome-chip" disabled={!ready} onClick={() => void go(p)}>{p}</button>
              ))}
            </div>
            <form className="welcome-row" onSubmit={(e) => { e.preventDefault(); void go(prompt); }}>
              <input className="welcome-field grow" aria-label="Your first prompt" placeholder="Or describe a task…"
                value={prompt} onChange={(e) => setPrompt(e.target.value)} />
              <button className="btn-primary" disabled={!ready || !prompt.trim()}>{starting ? "Starting…" : "Start"}</button>
            </form>
            {setup !== null && !keyed.length && <p className="welcome-note">Connect a model first.</p>}
          </li>
        </ol>

        {err && <p className="welcome-err" role="alert">{err}</p>}

        <section className="welcome-how" aria-label="How bough works">
          <h2>How it works</h2>
          <ul>
            <li><b>One program per step.</b> The model writes JavaScript that calls <code>tools.view</code>, <code>tools.patch</code> and <code>tools.bash</code>, so it batches work and branches on results instead of making one call per round trip.</li>
            <li><b>Edits stay in the checkout.</b> File tools write only inside the git checkout a session started in. The shell runs as you, so keep work committed.</li>
            <li><b>Everything is a plugin.</b> The provider, tools, hooks and skills are rows in <code>bough.yml</code>. The terminal UI (<code>bough</code>) and this page drive the same sessions.</li>
          </ul>
        </section>

        <p className="welcome-foot"><button className="link" onClick={() => { dismiss(); onSkip(); }}>Skip the welcome</button></p>
      </div>
    </div>
  );
}
