// The one bough window. Left heads · center conversation · right context rail, with the
// heads map and bundle browser as full-window views layered over it.
//
// Two top components share the same presentational Window: MockApp (static fixtures, no
// network — the default while the backend is offline) and LiveApp (store + SSE + REST
// against :4321). Flip with VITE_MOCK=false. The Changes tab stays on mock diffs until
// the Diff schema lands; everything else in LiveApp is real.
import { useCallback, useEffect, useRef, useState } from "react";
import { c } from "./theme";
import * as mock from "./mock";
import { api, type ModelOption, type SkillInfo } from "./api";
import { useStore } from "./store";
import { bundleFromSummary, diffsToFiles, headGroupsFromSessions, outlineFromThread } from "./live";
import type { HeadGroup } from "./live";
import type { Bundle, ActivityGroup, DiffFile, OutlineNode } from "./mock";
import type { ChangeSource, Message, NetRequest, Session } from "./types";
import { Conversation } from "./components/Conversation";
import { DiffViewer } from "./components/DiffViewer";
import { LeftRail } from "./components/LeftRail";
import { CommandPalette } from "./components/CommandPalette";
import { RightRail, type RailTab } from "./components/RightRail";
import { MapView } from "./components/MapView";
import { LiveMapView } from "./components/LiveMapView";
import { BundleBrowser } from "./components/BundleBrowser";
import { TitleBar, StatusStrip } from "./components/TitleBar";
import { useIsMobile } from "./useIsMobile";
import { Dot, Logo } from "./components/ui";

// Live is the default — this app is meant to daily-drive its own backend. Mock mode
// (static fixtures, no network) is the explicit opt-in for offline design review:
// build/run with VITE_MOCK=true.
const USE_MOCK = import.meta.env.VITE_MOCK === "true";

type View = "main" | "map" | "bundles";

// Deep-link the screens by URL hash so each state is reachable for review/screenshots:
// #main #network (focused) #changes #map #bundles.
function readHash(): { view: View; tab: RailTab; wide: boolean; file: string | null } {
  const h = location.hash.replace(/^#/, "");
  if (h === "map") return { view: "map", tab: "network", wide: false, file: null };
  if (h === "bundles") return { view: "bundles", tab: "network", wide: false, file: null };
  if (h === "changes") return { view: "main", tab: "changes", wide: false, file: "auth/token.js" };
  if (h === "network") return { view: "main", tab: "network", wide: true, file: null };
  return { view: "main", tab: "network", wide: false, file: null };
}

export default function App() {
  return USE_MOCK ? <MockApp /> : <LiveApp />;
}

// ---- mock path (default; no backend needed) -------------------------------
function MockApp() {
  return (
    <Window
      live={false}
      connected
      title="main · migrate-auth"
      sessions={mock.sessions}
      currentId={null}
      groups={[{ key: "~/repos/app", label: "app", workspace: "~/repos/app", heads: mock.heads }]}
      outline={mock.outline}
      thread={mock.thread}
      streaming={{}}
      activity={mock.activity}
      netStatus={{ enabled: true, running: true, listeners: 1, caPath: "~/.bough/net/ca/ca.crt" }}
      net={mock.net}
      pending={mock.pending}
      onResolve={() => {}}
      policy={{
        mode: "review",
        allowHosts: ["api.github.com", "registry.npmjs.org"],
        denyHosts: [],
        hostMiss: "hold",
        k8sHosts: [],
        allowVerbs: [],
        denyVerbs: [],
        holdVerbs: [],
      }}
      onSavePolicy={() => {}}
      diffs={mock.diffs}
      bundles={mock.bundles}
      notice={null}
      onSend={() => {}}
      onSelectHead={() => {}}
      onInstallBundle={() => {}}
      onApplyFile={() => {}}
      onApplyAll={() => {}}
      onRevert={() => {}}
    />
  );
}

// ---- live path (VITE_MOCK=false; talks to the Deno backend) ----------------
function LiveApp() {
  const store = useStore();
  const [bundles, setBundles] = useState<Bundle[]>([]);
  const [model, setModel] = useState("");
  const [models, setModels] = useState<ModelOption[]>([]);
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [paletteOpen, setPaletteOpen] = useState(false);
  // Bumping this asks the LeftRail to open its new-session form (from the palette).
  const [newSessionSignal, setNewSessionSignal] = useState(0);
  const bootstrapped = useRef(false);

  useEffect(() => {
    api.config().then((c) => {
      setModel(c.model);
      setModels(c.models);
    }).catch(() => {});
    api.skills().then(setSkills).catch(() => {});
  }, []);

  // ⌘K / ⌘P (and ctrl variants) toggle the command palette — the keyboard spine.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === "k" || e.key === "p")) {
        e.preventDefault();
        setPaletteOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Ensure a session is open so the composer has somewhere to post. Create "main" on a
  // fresh install, otherwise open the most recent session.
  useEffect(() => {
    if (bootstrapped.current) return;
    bootstrapped.current = true;
    (async () => {
      const list = await api.listSessions();
      const target = list[0] ?? (await api.createSession({ title: "main" }));
      await store.open(target.id);
      // Background subagents in flight → orient on the map (the agent overview),
      // not wherever the last thread happened to be.
      if (list.some((s) => s.kind === "subagent" && s.busy)) location.hash = "map";
    })().catch(() => {});
  }, [store]);

  const loadBundles = useCallback(async () => {
    const list = await api.listBundles();
    setBundles(list.map(bundleFromSummary));
  }, []);
  useEffect(() => {
    loadBundles().catch(() => {});
  }, [loadBundles]);

  const onInstallBundle = useCallback(
    (id: string) => {
      api.installBundle(id, {}).then(loadBundles).catch(() => {});
    },
    [loadBundles]
  );

  const diffs = diffsToFiles(store.changes);

  // Apply all: one call per snapshot source, with that source's file paths.
  const onApplyAll = () => {
    for (const source of ["jj", "clonefile"] as ChangeSource[]) {
      const paths = diffs.filter((f) => f.source === source).map((f) => f.path);
      if (paths.length) store.applyChanges(source, paths);
    }
  };

  // Composer submit. While a turn runs: ↵ steers (posts now, the live turn yields at
  // its next round boundary) and ⌥↵ queues (stages until the turn finishes). While
  // idle: ⌥↵ is "branch here" — fork from the last user turn, resending with this
  // text, a variation from the same point that preserves the current head.
  const onSend = (text: string, alt: boolean) => {
    if (store.busy) return void store.send(text, alt);
    if (!alt) return void store.send(text);
    const lastUser = [...store.thread].reverse().find((m) => m.role === "user");
    if (lastUser) store.fork(lastUser.id, text);
    else store.send(text);
  };

  return (
    <>
      <Window
        live
        connected={store.connected}
        title={store.session?.title ?? "session"}
        sessions={store.sessions}
        currentId={store.currentId}
        groups={headGroupsFromSessions(store.sessions, store.currentId)}
        outline={outlineFromThread(store.thread)}
        thread={store.thread}
        streaming={store.streaming}
        activity={[]}
        netStatus={store.netStatus}
        net={store.net}
        pending={store.pending}
        pendingCount={store.pendingCount}
        onResolve={store.resolvePending}
        policy={store.policy}
        policySource={store.policySource}
        onSavePolicy={store.savePolicy}
        onOverridePolicy={store.overridePolicy}
        onClearPolicyOverride={store.clearPolicyOverride}
        onPolicyChanged={store.refreshPolicy}
        diffs={diffs}
        bundles={bundles}
        notice={store.notice}
        busy={store.busy}
        model={model}
        models={models}
        usage={store.usage}
        onSetModel={(m) => api.setModel(m).then((r) => setModel(r.model)).catch(() => {})}
        workspace={store.session?.workspace ?? null}
        newSessionSignal={newSessionSignal}
        onSend={onSend}
        onInterrupt={store.interrupt}
        onSearchFiles={(q) => (store.currentId ? api.searchFiles(store.currentId, q) : Promise.resolve([]))}
        skills={skills}
        onSelectHead={(id) => store.open(id)}
        onInstallBundle={onInstallBundle}
        onApplyFile={(f) => f.source && store.applyChanges(f.source, [f.path])}
        onApplyAll={onApplyAll}
        onRevert={() => store.revertChanges()}
        onAdopt={store.adopt}
        onCreateSession={(workspace) => store.newSession(workspace || undefined)}
        onArchiveHead={(id) => store.archive(id)}
        onForkEdit={(id, text) => store.fork(id, text)}
        onCompact={(fromId, toId, sessionId) => store.compact(fromId, toId, sessionId)}
        onDismissNotice={store.dismissNotice}
        queued={store.queued}
        onRemoveQueued={store.removeQueued}
        onEditQueued={store.editQueued}
      />
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        sessions={store.sessions}
        currentId={store.currentId}
        busy={store.busy}
        onOpenSession={(id) => {
          location.hash = "main";
          store.open(id);
        }}
        onInterrupt={store.interrupt}
        onNewSession={() => {
          location.hash = "main";
          setNewSessionSignal((n) => n + 1);
        }}
        onMap={() => (location.hash = "map")}
        onBundles={() => (location.hash = "bundles")}
      />
    </>
  );
}

// ---- shared window shell + screen routing ---------------------------------
function Window({
  live,
  connected,
  title,
  sessions,
  currentId,
  groups,
  outline,
  thread,
  streaming,
  activity,
  netStatus,
  net,
  pending,
  pendingCount,
  onResolve,
  policy,
  policySource,
  onSavePolicy,
  onOverridePolicy,
  onClearPolicyOverride,
  onPolicyChanged,
  diffs,
  bundles,
  notice,
  busy = false,
  model,
  models,
  usage,
  onSetModel,
  workspace,
  onSend,
  onInterrupt,
  onSearchFiles,
  skills,
  onSelectHead,
  onInstallBundle,
  onApplyFile,
  onApplyAll,
  onRevert,
  onAdopt,
  onCreateSession,
  onArchiveHead,
  newSessionSignal,
  onForkEdit,
  onCompact,
  onDismissNotice,
  queued = [],
  onRemoveQueued,
  onEditQueued,
}: {
  live: boolean;
  connected: boolean;
  title: string;
  sessions: Session[];
  currentId: string | null;
  groups: HeadGroup[];
  outline: OutlineNode[];
  thread: Message[];
  streaming: Record<string, string>;
  activity: ActivityGroup[];
  netStatus: import("./api").NetStatus;
  net: NetRequest[];
  pending: NetRequest | null;
  pendingCount?: number;
  onResolve: (approve: boolean) => void;
  policy: import("./api").NetConfig | null;
  policySource?: import("./api").PolicySource | null;
  onSavePolicy: (cfg: import("./api").NetConfig) => void;
  onOverridePolicy?: () => void;
  onClearPolicyOverride?: () => void;
  onPolicyChanged?: () => void;
  diffs: DiffFile[];
  bundles: Bundle[];
  notice?: string | null;
  busy?: boolean;
  model?: string;
  models?: ModelOption[];
  usage?: { contextTokens: number; outputTokens: number };
  onSetModel?: (model: string) => void;
  workspace?: string | null;
  onSend: (text: string, branch: boolean) => void;
  onInterrupt?: () => void;
  onSearchFiles?: (q: string) => Promise<string[]>;
  skills?: SkillInfo[];
  onSelectHead: (id: string) => void;
  onInstallBundle: (id: string) => void;
  onApplyFile: (file: DiffFile) => void;
  onApplyAll: () => void;
  onRevert: () => void;
  // Adopt the open subagent branch's changes into its spawner (see RightRail).
  onAdopt?: () => void;
  onCreateSession?: (workspace: string) => void;
  onArchiveHead?: (id: string) => void;
  newSessionSignal?: number;
  onForkEdit?: (messageId: string, text: string) => void;
  onCompact?: (fromId: string, toId: string, sessionId?: string) => void;
  onDismissNotice?: () => void;
  queued?: string[];
  onRemoveQueued?: (i: number) => void;
  onEditQueued?: (i: number, text: string) => void;
}) {
  const init = readHash();
  const [view, setView] = useState<View>(init.view);
  const [tab, setTab] = useState<RailTab>(init.tab);
  const [wide, setWide] = useState(init.wide);
  const [selectedFile, setSelectedFile] = useState<string | null>(init.file);
  // Subagent glanceables: is the open session a subagent branch (adopt affordance,
  // role label), and how many subagents are running right now (title-bar badge).
  const currentIsSubagent = sessions.find((s) => s.id === currentId)?.kind === "subagent";
  const subagentsRunning = sessions.filter((s) => s.kind === "subagent" && s.busy).length;
  // Phone layout: the rails leave the flow and become tap-to-open drawers.
  const mobile = useIsMobile();
  const [leftOpen, setLeftOpen] = useState(false);
  const [rightOpen, setRightOpen] = useState(false);

  useEffect(() => {
    const onHash = () => {
      const s = readHash();
      setView(s.view);
      setTab(s.tab);
      setWide(s.wide);
      setSelectedFile(s.file);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  // Selected file for the diff viewer; fall back to the first file so the Changes tab
  // always shows something when there are changes.
  const diffFile = diffs.find((f) => f.path === selectedFile) ?? diffs[0] ?? null;

  function openTab(t: RailTab) {
    setTab(t);
    if (t === "changes" && !diffs.some((f) => f.path === selectedFile)) {
      setSelectedFile(diffs[0]?.path ?? null);
    }
  }

  if (view === "map")
    return (
      <Shell>
        {live ? (
          <LiveMapView
            sessions={sessions}
            currentId={currentId}
            onJump={(id) => {
              onSelectHead(id);
              location.hash = "main";
            }}
            onCompact={(sessionId, fromId, toId) => onCompact?.(fromId, toId, sessionId)}
            onClose={() => (location.hash = "main")}
          />
        ) : (
          <MapView onClose={() => (location.hash = "main")} />
        )}
      </Shell>
    );
  if (view === "bundles")
    return (
      <Shell>
        <BundleBrowser bundles={bundles} onInstall={onInstallBundle} onClose={() => (location.hash = "main")} />
      </Shell>
    );

  const showDiff = tab === "changes" && !!diffFile;
  const dimConversation = false;

  const leftRail = (
    <LeftRail
      groups={groups}
      outline={outline}
      openFormSignal={newSessionSignal}
      onOpenMap={() => (location.hash = "map")}
      onSelectHead={(id) => {
        onSelectHead(id);
        setLeftOpen(false);
      }}
      onCreateSession={onCreateSession && ((w) => {
        onCreateSession(w);
        setLeftOpen(false);
      })}
      onArchiveHead={onArchiveHead}
    />
  );
  const rightRail = (
    <RightRail
      tab={tab}
      onTab={openTab}
      wide={wide}
      onToggleWide={mobile ? undefined : () => setWide((w) => !w)}
      netStatus={netStatus}
      net={net}
      pending={pending}
      pendingCount={pendingCount}
      onResolve={onResolve}
      policy={policy}
      policySource={policySource}
      onSavePolicy={onSavePolicy}
      onOverridePolicy={onOverridePolicy}
      onClearPolicyOverride={onClearPolicyOverride}
      onPolicyChanged={onPolicyChanged}
      sessionOpen={currentId !== null}
      sessionId={currentId}
      diffs={diffs}
      onAdopt={currentIsSubagent ? onAdopt : undefined}
      selectedFile={diffFile?.path ?? null}
      onSelectFile={(p) => {
        setSelectedFile(p);
        setRightOpen(false);
      }}
      onApplyAll={onApplyAll}
      onRevert={onRevert}
    />
  );

  return (
    <Shell>
      <div style={{ display: "flex", flexDirection: "column", height: "100%", background: c.panel }}>
        {mobile
          ? (
            <MobileBar
              title={title}
              connected={connected}
              live={live}
              busy={busy}
              attentionCount={sessions.filter((s) => s.unseen).length}
              pendingCount={pendingCount ?? (pending ? 1 : 0)}
              onBack={showDiff ? () => openTab("network") : undefined}
              onMenu={() => setLeftOpen(true)}
              onRail={() => setRightOpen(true)}
            />
          )
          : (
            <TitleBar
              branch={title}
              live={live}
              connected={connected}
              model={model}
              models={models}
              usage={usage}
              onSetModel={onSetModel}
              workspace={workspace}
              sessionId={currentId}
              subagentsRunning={subagentsRunning}
              onShowMap={() => (location.hash = "map")}
            />
          )}
        <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
          {!mobile && leftRail}

          {showDiff ? (
            <DiffViewer
              file={diffFile}
              live={live}
              onApplyFile={() => diffFile && onApplyFile(diffFile)}
              onApplyHunk={() => {}}
              onSkipHunk={() => {}}
            />
          ) : (
            <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0 }}>
              {notice && (
                <div
                  style={{
                    flex: "none",
                    display: "flex",
                    alignItems: "center",
                    gap: 10,
                    padding: "9px 34px",
                    background: "rgba(226,119,110,.10)",
                    borderBottom: `1px solid ${c.border}`,
                    color: c.red,
                    fontSize: 12.5,
                  }}
                >
                  <span style={{ flex: 1 }}>{notice}</span>
                  <button onClick={onDismissNotice} style={{ color: c.muted, fontSize: 14 }}>✕</button>
                </div>
              )}
              <Conversation
                thread={thread}
                streaming={streaming}
                focusKey={currentId}
                activity={activity}
                subagents={sessions.filter((s) => s.kind === "subagent")}
                onOpenSession={onSelectHead}
                subagentThread={currentIsSubagent}
                dimmed={dimConversation}
                canBranch={live}
                busy={busy}
                onSend={onSend}
                onInterrupt={onInterrupt}
                onSearchFiles={onSearchFiles}
                skills={skills}
                onForkEdit={onForkEdit}
                onCompact={onCompact}
                queued={queued}
                onRemoveQueued={onRemoveQueued}
                onEditQueued={onEditQueued}
                disabled={false}
              />
            </div>
          )}

          {!mobile && rightRail}
        </div>
        {!mobile && (
          <StatusStrip heads={groups.reduce((n, g) => n + g.heads.length, 0)} live={live} connected={connected} />
        )}
      </div>
      {mobile && leftOpen && <Drawer side="left" onClose={() => setLeftOpen(false)}>{leftRail}</Drawer>}
      {mobile && rightOpen && <Drawer side="right" onClose={() => setRightOpen(false)}>{rightRail}</Drawer>}
    </Shell>
  );
}

// ---- phone chrome -----------------------------------------------------------

// Slim top bar for phones: heads drawer · title · connection dot · context drawer
// (badged while a network hold waits). Replaces the desktop TitleBar + StatusStrip.
// Count badge pinned to a bar button's corner (network holds, unseen sessions).
function BarBadge({ count, color }: { count: number; color: string }) {
  if (count <= 0) return null;
  return (
    <span
      style={{
        position: "absolute",
        top: -5,
        right: -5,
        minWidth: 16,
        height: 16,
        borderRadius: 8,
        background: color,
        color: c.bg,
        fontSize: 10,
        fontWeight: 700,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: "0 4px",
      }}
    >
      {count}
    </span>
  );
}

function MobileBar({
  title,
  connected,
  live,
  busy,
  attentionCount,
  pendingCount,
  onBack,
  onMenu,
  onRail,
}: {
  title: string;
  connected: boolean;
  live: boolean;
  // The open session has a turn running — pulse next to the title.
  busy: boolean;
  // Sessions with a finished turn nobody has looked at — badge on the ☰ button.
  attentionCount: number;
  pendingCount: number;
  // Set while the diff viewer fills the screen — returns to the conversation.
  onBack?: () => void;
  onMenu: () => void;
  onRail: () => void;
}) {
  const barButton: React.CSSProperties = {
    minWidth: 40,
    height: 34,
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    border: `1px solid ${c.border}`,
    borderRadius: 8,
    color: c.muted,
    fontSize: 15,
    position: "relative",
  };
  return (
    <div
      style={{
        flex: "none",
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "8px 10px",
        paddingTop: "calc(8px + env(safe-area-inset-top))",
        background: c.panel3,
        borderBottom: `1px solid ${c.border}`,
      }}
    >
      {onBack
        ? <button onClick={onBack} style={barButton}>‹</button>
        : (
          <button onClick={onMenu} title="Sessions" style={barButton}>
            ☰
            <BarBadge count={attentionCount} color={c.green} />
          </button>
        )}
      <Logo size={15} />
      <span
        style={{
          flex: 1,
          fontSize: 13.5,
          fontWeight: 600,
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {title}
      </span>
      {busy && (
        <span
          className="pulse-green"
          title="a turn is running"
          style={{ flex: "none", width: 8, height: 8, borderRadius: "50%", background: c.panel, border: `2px solid ${c.green}` }}
        />
      )}
      {live && <Dot color={connected ? c.green : c.muted2} pulse={connected} />}
      <button onClick={onRail} title="Network & changes" style={barButton}>
        ▤
        <BarBadge count={pendingCount} color={c.amber} />
      </button>
    </div>
  );
}

// Edge drawer over a dimmed backdrop; tap outside to close. Hosts the untouched
// desktop rails on phones.
function Drawer({ side, onClose, children }: { side: "left" | "right"; onClose: () => void; children: React.ReactNode }) {
  return (
    <div
      onClick={onClose}
      style={{ position: "fixed", inset: 0, zIndex: 40, background: "rgba(0,0,0,.55)" }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          position: "absolute",
          top: 0,
          bottom: 0,
          [side]: 0,
          maxWidth: "88vw",
          display: "flex",
          boxShadow: "0 0 40px rgba(0,0,0,.5)",
        }}
      >
        {children}
      </div>
    </div>
  );
}

// The app fills the OS webview / browser viewport as one window. 100dvh (not vh)
// tracks the real visible height on phones, where the URL bar and the on-screen
// keyboard resize the viewport out from under a fixed 100vh.
function Shell({ children }: { children: React.ReactNode }) {
  return <div style={{ height: "100dvh", width: "100vw", overflow: "hidden", background: c.bg }}>{children}</div>;
}
