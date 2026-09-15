// The first thing someone sees on an empty server: add a provider key,
// pick a folder the agent can edit, ask for something. It reads the same
// facts a session acts on (/api/setup), so the page cannot promise a
// writable folder the launcher then refuses. Before this, a first visit was
// an empty "Nothing needs your attention" and a New button that started a
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
  "Explain this repo",
  "Run the tests and fix what fails",
  "Review uncommitted changes",
];

const LABEL: Record<string, string> = { anthropic: "Anthropic", openrouter: "OpenRouter", openai: "OpenAI", cerebras: "Cerebras" };
const label = (name: string) => LABEL[name] ?? name;

const tilde = (p: string, home: string) => (home && (p === home || p.startsWith(home + "/")) ? "~" + p.slice(home.length) : p);

/** The folder check for the path as typed now: never a result for a path that was typed before. */
type Check = { state: "idle" | "checking" | "failed" } | { state: "done"; folder: SetupFolder };

export function Welcome({ onStart, onSkip }: { onStart: (cwd: string, prompt: string) => Promise<unknown> | void; onSkip: () => void }) {
  const [setup, setSetup] = useState<Setup | null>(null);
  const [loadErr, setLoadErr] = useState("");
  const [providers, setProviders] = useState<SetupProvider[]>([]);
  const [prov, setProv] = useState("anthropic");
  const [key, setKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [keyErr, setKeyErr] = useState("");
  const [path, setPath] = useState("");
  const [check, setCheck] = useState<Check>({ state: "idle" });
  const [prompt, setPrompt] = useState("");
  const [starting, setStarting] = useState(false);
  const [startErr, setStartErr] = useState("");

  useEffect(() => {
    api.setup().then((s) => {
      setSetup(s);
      setProviders(s.providers);
      setPath(tilde(s.folder.path, s.home));
    }).catch((e: Error) => setLoadErr(e.message));
  }, []);

  // Each edit re-asks the server, so the line under the field is what a
  // session started there would actually get. The previous answer is
  // dropped the moment the path changes: a stale "checkout found" under a
  // new path is exactly the reassurance this step must not give.
  useEffect(() => {
    if (!setup) return;
    const p = path.trim();
    if (!p) { setCheck({ state: "idle" }); return; }
    setCheck({ state: "checking" });
    let on = true;
    const t = setTimeout(() => {
      api.setup(p)
        .then((s) => { if (on) setCheck({ state: "done", folder: s.folder }); })
        .catch(() => { if (on) setCheck({ state: "failed" }); });
    }, 250);
    return () => { on = false; clearTimeout(t); };
  }, [path, setup]);

  const home = setup?.home ?? "";
  const keyed = providers.filter((p) => p.set);
  const folder = check.state === "done" ? check.folder : null;
  const ready = keyed.length > 0 && !!folder?.exists && !starting;

  const save = async () => {
    setSaving(true);
    setKeyErr("");
    try {
      setProviders(await api.setKey(prov, key));
      setKey("");
    } catch (e) {
      setKeyErr((e as Error).message);
    } finally {
      setSaving(false);
    }
  };

  const go = async (text: string) => {
    if (!ready || !folder || !text.trim()) return;
    setStarting(true);
    setStartErr("");
    try {
      dismiss();
      await onStart(folder.path, text.trim());
    } catch (e) {
      setStartErr((e as Error).message);
      setStarting(false);
    }
  };

  let status: { text: string; tone: "ok" | "warn" | "err" | "" };
  if (!path.trim()) status = { text: "Type the path of a project folder.", tone: "" };
  else if (check.state === "failed") status = { text: "Couldn’t check this folder. Try again.", tone: "err" };
  else if (!folder) status = { text: "Checking folder…", tone: "" };
  else if (!folder.exists) status = { text: "There is no folder at this path.", tone: "warn" };
  else if (folder.checkout) {
    const at = folder.checkout === folder.path ? "" : ` at ${tilde(folder.checkout, home)}`;
    status = { text: `Git checkout found${at}. File tools can edit it; shell commands run with your user permissions.`, tone: "ok" };
  } else status = { text: "Not a git checkout: the agent can read files here but not edit them. Pick a repo to let it make changes.", tone: "warn" };

  let hint = "";
  if (setup !== null && !keyed.length) hint = "Add a provider key to enable these.";
  else if (folder && !folder.exists) hint = "Pick a folder that exists.";
  else if (ready) hint = "Choosing a suggestion starts a session right away.";

  return (
    <div className="welcome scroll">
      <div className="welcome-inner">
        <header className="welcome-head">
          <h1>Welcome to bough</h1>
          <p>A coding agent that writes one JavaScript program per step.</p>
        </header>

        <ol className="welcome-steps">
          <li className={"welcome-step" + (keyed.length ? " done" : "")}>
            <h2><span className="welcome-num" aria-hidden="true">1</span>Add a provider key</h2>
            {setup === null ? (
              <p className="welcome-note">{loadErr ? `Couldn’t check for keys: ${loadErr}` : "Checking for keys…"}</p>
            ) : keyed.length ? (
              <p className="welcome-status ok">Key found for {keyed.map((p) => label(p.name)).join(", ")}. Pick a model inside the session.</p>
            ) : (<>
              <p className="welcome-note">
                Saved to <code>{tilde(setup.envFile, home)}</code> on the machine running bough, and sent only to {label(prov)}.
              </p>
              <form className="welcome-key-row" onSubmit={(e) => { e.preventDefault(); void save(); }}>
                <select className="welcome-field" aria-label="Provider" value={prov} onChange={(e) => setProv(e.target.value)}>
                  {providers.map((p) => <option key={p.name} value={p.name}>{label(p.name)}</option>)}
                </select>
                <input className="welcome-field" type="password" aria-label={`${label(prov)} API key`} placeholder="API key"
                  autoComplete="off" spellCheck={false} value={key} onChange={(e) => setKey(e.target.value)} />
                <button className="btn btn-primary" disabled={!key.trim() || saving}>{saving ? "Saving…" : "Save key"}</button>
              </form>
              {keyErr && <p className="welcome-status err" role="alert">{keyErr}</p>}
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
              <button className="btn btn-primary" disabled={!ready || !prompt.trim()}>{starting ? "Starting…" : "Start"}</button>
            </form>
            {hint && <p className="welcome-note">{hint}</p>}
            {startErr && <p className="welcome-status err" role="alert">{startErr}</p>}
          </li>
        </ol>

        <section className="welcome-how" aria-label="How bough works">
          <h2>How it works</h2>
          <ul>
            <li><b>One program per step.</b> The model writes JavaScript that calls <code>tools.view</code>, <code>tools.patch</code> and <code>tools.bash</code>, so it batches work and branches on results instead of making one call per round trip.</li>
            <li><b>File tools are checkout-scoped.</b> <code>tools.write</code> and <code>tools.patch</code> refuse paths outside the checkout. The shell runs as you, so keep work committed.</li>
            <li><b>Everything is a plugin.</b> The provider, tools, hooks and skills are rows in <code>bough.yml</code>. The terminal UI (<code>bough</code>) and this page drive the same sessions.</li>
          </ul>
        </section>

        <p className="welcome-foot"><button className="link" onClick={() => { dismiss(); onSkip(); }}>Skip the welcome</button></p>
      </div>
    </div>
  );
}
