// The first thing someone sees on an empty server: add a provider key,
// pick a folder the agent can edit, ask for something. It reads the same
// facts a session acts on (/api/setup), so the page cannot promise a
// writable folder the launcher then refuses. Before this, a first visit was
// an empty "Nothing needs your attention" and a New button that started a
// read-only session in home.
import { useEffect, useState } from "react";
import { api, type KeyState, type Setup, type SetupFolder, type SetupProvider } from "./api";

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

export type KeyChecks = Record<string, KeyState | "checking">;

/**
 * What step 1 says about the keys that are set. "Key found" alone was a
 * promise: a key the provider rejects failed every session with a 401.
 * A key that could not be checked (offline) still counts as usable.
 */
export function keyLine(providers: SetupProvider[], checks: KeyChecks): { text: string; tone: "ok" | "warn" | "err" | ""; working: boolean } {
  const set = providers.filter((p) => p.set);
  if (set.some((p) => (checks[p.name] ?? "checking") === "checking")) return { text: "Checking your keys…", tone: "", working: false };
  const good = set.filter((p) => checks[p.name] === "ok" || checks[p.name] === "unknown");
  const bad = set.filter((p) => checks[p.name] === "rejected");
  const names = (l: SetupProvider[]) => l.map((p) => label(p.name)).join(", ");
  const parts: string[] = [];
  if (good.length) parts.push(`${names(good)} key ${good.length > 1 ? "work" : "works"}.`);
  if (bad.length) parts.push(`${names(bad)} key was rejected: replace it${good.length ? " or pick a working provider’s model in the session" : ""}.`);
  if (good.length && !bad.length) parts.push("Pick a model inside the session.");
  return { text: parts.join(" "), tone: bad.length ? (good.length ? "warn" : "err") : "ok", working: good.length > 0 };
}

/** The provider the key form opens on: a rejected key to replace, else one with no key. */
export function pickProvider(providers: SetupProvider[], checks: KeyChecks): string {
  return (providers.find((p) => p.set && checks[p.name] === "rejected") ?? providers.find((p) => !p.set) ?? providers[0])?.name ?? "anthropic";
}

/** The folder check for the path as typed now: never a result for a path that was typed before. */
type Check = { state: "idle" | "checking" | "failed" } | { state: "done"; folder: SetupFolder };

export function Welcome({ onStart, onSkip, onBack }: {
  onStart: (cwd: string, prompt: string) => Promise<unknown> | void; onSkip: () => void;
  /** A phone's way to the session list; the welcome filled the screen with no nav. */
  onBack?: () => void;
}) {
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
  const [checks, setChecks] = useState<KeyChecks>({});

  const loadSetup = () => {
    setLoadErr("");
    api.setup().then((s) => {
      setSetup(s);
      setProviders(s.providers);
      setPath(tilde(s.folder.path, s.home));
    }).catch((e: Error) => setLoadErr(e.message));
  };
  useEffect(loadSetup, []);
  // Every set key is asked once per change of the provider list (a saved key re-asks).
  useEffect(() => {
    let on = true;
    const set = providers.filter((p) => p.set);
    setChecks(Object.fromEntries(set.map((p) => [p.name, "checking"])));
    for (const p of set) {
      api.checkKey(p.name).then((r) => r.state, () => "unknown" as const)
        .then((st) => { if (on) setChecks((c) => ({ ...c, [p.name]: st })); });
    }
    return () => { on = false; };
  }, [providers]);
  const keys = keyLine(providers, checks);
  useEffect(() => { if (!keys.text.startsWith("Checking")) setProv(pickProvider(providers, checks)); }, [keys.text]); // eslint-disable-line react-hooks/exhaustive-deps
  // Bumped by Retry, so an unchanged path can be checked again.
  const [checkRev, setCheckRev] = useState(0);

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
  }, [path, setup, checkRev]);

  const home = setup?.home ?? "";
  const keyed = providers.filter((p) => p.set);
  const checking = keyed.length > 0 && keys.text.startsWith("Checking");
  const rejected = keyed.length > 0 && !checking && keys.tone !== "ok";
  const folder = check.state === "done" ? check.folder : null;
  const ready = keys.working && !!folder?.exists && !starting;

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
      await onStart(folder.path, text.trim());
      // Only once a session exists: a failed start must leave the welcome
      // there on the next visit.
      dismiss();
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
  } else status = { text: "Not a git checkout: file tools can’t edit here, but shell commands still run with your user permissions and can change files. Pick a repo to let the agent make edits.", tone: "warn" };

  let hint = "";
  if (setup !== null && !checking && !keys.working) hint = keyed.length ? "Add a key the provider accepts to enable these." : "Add a provider key to enable these.";
  else if (folder && !folder.exists) hint = "Pick a folder that exists.";
  else if (ready) hint = "Choosing a suggestion starts a session right away.";

  return (
    <div className="welcome scroll">
      {onBack && (
        <nav className="welcome-nav" aria-label="Welcome">
          <button type="button" className="back" onClick={onBack} aria-label="Back to sessions">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"
                 strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M15 5l-7 7 7 7" /></svg>
          </button>
          <span className="welcome-nav-title">Sessions</span>
          <button type="button" className="link welcome-nav-skip" onClick={() => { dismiss(); onSkip(); }}>Skip</button>
        </nav>
      )}
      <div className="welcome-inner">
        <header className="welcome-head">
          <h1>Welcome to bough</h1>
          <p>A coding agent that writes one JavaScript program per step.</p>
        </header>

        <ol className="welcome-steps">
          <li className={"welcome-step" + (keys.working ? " done" : "")}>
            <h2><span className="welcome-num" aria-hidden="true">1</span>Add a provider key</h2>
            {setup === null ? (
              loadErr ? (<>
                <p className="welcome-status err" role="alert">Couldn’t load setup: {loadErr}</p>
                <button className="btn welcome-retry" onClick={loadSetup}>Retry</button>
              </>) : <p className="welcome-note">Checking for keys…</p>
            ) : keyed.length && !rejected ? (
              <p className={"welcome-status " + keys.tone} role="status">{keys.text}</p>
            ) : (<>
              {rejected && <p className={"welcome-status " + keys.tone} role="alert">{keys.text}</p>}
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
            {check.state === "failed" && <button className="btn welcome-retry" onClick={() => setCheckRev((n) => n + 1)}>Retry</button>}
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

        <p className="welcome-foot"><button className="link welcome-skip" onClick={() => { dismiss(); onSkip(); }}>Skip the welcome</button></p>
      </div>
    </div>
  );
}
