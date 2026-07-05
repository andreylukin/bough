// App state + event reduction. Holds the session list, the open thread, per-message
// streaming buffers, and the network feed. Everything the UI renders derives from here.
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type NetConfig, type NetStatus, type PolicySource, readBranch, type Usage } from "./api";
import { useEvents } from "./useEvents";
import type { BoughEvent, ChangeSource, Message, NetRequest, Part, Session, WireDiff } from "./types";

export interface Store {
  sessions: Session[];
  currentId: string | null;
  session: Session | null;
  thread: Message[];
  // messageId -> live text accumulated from message.delta, shown until the finalized
  // text part lands (then cleared to avoid double-rendering).
  streaming: Record<string, string>;
  connected: boolean;
  // A turn is streaming for the open session (any of its messages still pending).
  busy: boolean;
  netStatus: NetStatus;
  // The live egress feed (newest first) and the current hold awaiting approval, if any.
  net: NetRequest[];
  pending: NetRequest | null;
  // How many holds are waiting in total (the card shows them one at a time).
  pendingCount: number;
  // The editable rule set (the open branch's effective one); null until loaded.
  policy: NetConfig | null;
  // Where that rule set came from: this branch / inherited ancestor / global.
  policySource: PolicySource | null;
  // Review payloads for the open session (0..2: jj repo + clonefile config).
  changes: WireDiff[];
  // A transient message to surface (e.g. a fork/compact 400); null when clear.
  notice: string | null;
  // Token usage for the open session (context meter); zeroed on session switch.
  usage: Usage;
  // Messages typed while a turn was running — staged, not yet sent. Editable/removable
  // until the turn finishes, then flushed. Cleared on session switch.
  queued: string[];
  removeQueued: (i: number) => void;
  editQueued: (i: number, text: string) => void;
  open: (id: string) => Promise<void>;
  newSession: (workspace?: string) => Promise<Session>;
  /** Post a message. While busy: posts immediately (steer) unless queue=true (stage until idle). */
  send: (text: string, queue?: boolean) => Promise<void>;
  interrupt: () => void;
  archive: (id: string) => void;
  resolvePending: (approve: boolean) => void;
  savePolicy: (cfg: NetConfig) => Promise<void>;
  overridePolicy: () => Promise<void>;
  clearPolicyOverride: () => Promise<void>;
  // Re-fetch the open branch's effective rules (e.g. after a plugin enable/disable
  // changed the row server-side, so a later rule-editor save doesn't clobber it).
  refreshPolicy: (sessionId?: string) => Promise<void>;
  reload: () => Promise<void>;
  refreshNetStatus: () => Promise<void>;
  applyChanges: (source: ChangeSource, paths: string[]) => void;
  revertChanges: () => void;
  fork: (atMessageId: string, editedText?: string) => void;
  // sessionId defaults to the open session (conversation compact); the map passes an
  // explicit head so a span on any lane compacts the right session.
  compact: (fromMessageId: string, toMessageId: string, sessionId?: string) => void;
  // Adopt the OPEN subagent session's changes into its spawner's workspace.
  adopt: () => void;
  dismissNotice: () => void;
}

export function useStore(): Store {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [session, setSession] = useState<Session | null>(null);
  const [thread, setThread] = useState<Message[]>([]);
  const [streaming, setStreaming] = useState<Record<string, string>>({});
  const [netStatus, setNetStatus] = useState<NetStatus>({
    enabled: false,
    running: false,
    listeners: 0,
    caPath: "",
  });
  const [net, setNet] = useState<NetRequest[]>([]);
  // ALL holds awaiting approval, oldest first — the card shows the head and the
  // next one surfaces automatically when it resolves (no refresh needed).
  const [pendings, setPendings] = useState<NetRequest[]>([]);
  const pending = pendings[0] ?? null;
  const [policy, setPolicy] = useState<NetConfig | null>(null);
  const [policySource, setPolicySource] = useState<PolicySource | null>(null);
  const [changes, setChanges] = useState<WireDiff[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [usage, setUsage] = useState<Usage>({ contextTokens: 0, outputTokens: 0 });
  const [queued, setQueued] = useState<string[]>([]);

  // currentId in a ref so the event handler (stable) can filter without re-subscribing.
  const currentRef = useRef<string | null>(null);
  currentRef.current = currentId;
  const policyRef = useRef<NetConfig | null>(null);
  policyRef.current = policy;
  const policySourceRef = useRef<PolicySource | null>(null);
  policySourceRef.current = policySource;

  // busy in a ref so `send` (a stable callback) can decide to stage vs. post without
  // re-subscribing. Set from the derived `busy` during render, below.
  const busyRef = useRef(false);

  // The held request, in a ref so resolvePending reads it without a fresh closure.
  const pendingRef = useRef<NetRequest | null>(null);
  pendingRef.current = pending;

  // refreshChanges in a ref so the stable event handler can call the latest one.
  const refreshChangesRef = useRef<(id: string | null) => void>(() => {});

  const reload = useCallback(async () => {
    const s = await api.listSessions();
    // `unseen` is client-side memory — carry it across the server refetch.
    setSessions((prev) => s.map((n) => ({ ...n, unseen: prev.find((p) => p.id === n.id)?.unseen })));
  }, []);

  // Proxy status for the Network rail (bough runs the firewall in-process).
  const refreshNetStatus = useCallback(async () => {
    setNetStatus(await api.netStatus());
  }, []);

  // Rebuild the net rail from the server, then live-update from `net.request` events.
  // The FEED is the open branch's egress only (a fresh session starts blank); with no
  // session open it falls back to the global feed. The hold-and-ask card stays GLOBAL:
  // a pending hold wedges its branch's turn wherever it is, and this UI is the only
  // place to release it.
  const refreshNet = useCallback(async (sessionId?: string) => {
    const id = sessionId ?? currentRef.current;
    const all = await api.netRequests();
    setNet(id ? await api.netRequests(id) : all);
    setPendings(all.filter((r) => r.verdict === "pending").reverse()); // oldest first
  }, []);

  // The rule set shown is the OPEN SESSION's effective one (own override, else the
  // nearest ancestor's, else global) so the rail reflects what its egress actually
  // gets. With no session open it falls back to the global rule set.
  const refreshPolicy = useCallback(async (sessionId?: string) => {
    const id = sessionId ?? currentRef.current;
    try {
      if (id) {
        const { config, source } = await api.getSessionPolicy(id);
        setPolicy(config);
        setPolicySource(source);
      } else {
        setPolicy(await api.getPolicy());
        setPolicySource({ scope: "global" });
      }
    } catch {
      setPolicy(null);
      setPolicySource(null);
    }
  }, []);

  const resolvePending = useCallback((approve: boolean) => {
    const req = pendingRef.current;
    // Drop this card optimistically — the NEXT parked hold (if any) surfaces right
    // away; the gate re-emits the row with its final verdict, reconciled by id.
    setPendings((prev) => prev.filter((p) => p.id !== req?.id));
    if (!req) return;
    (approve ? api.allowRequest(req.id) : api.denyRequest(req.id)).catch(() => {
      // The hold may have already resolved/expired server-side (404). Re-sync so the
      // rail reflects the true state rather than a stale card.
      refreshNet();
    });
  }, [refreshNet]);

  // Persist a new rule set; PUT hot-swaps the live gate. Optimistic, then reconcile.
  // Saves to the scope in effect: a branch that owns an override keeps editing its
  // override; a branch on inherited/global rules edits the global set (creating an
  // override is the explicit overridePolicy action below).
  const savePolicy = useCallback(async (cfg: NetConfig) => {
    const id = currentRef.current;
    const toBranch = id && policySourceRef.current?.scope === "session";
    setPolicy(cfg);
    try {
      if (toBranch) {
        const { config, source } = await api.putSessionPolicy(id, cfg);
        setPolicy(config);
        setPolicySource(source);
      } else {
        setPolicy(await api.putPolicy(cfg));
      }
    } catch {
      refreshPolicy();
    }
  }, [refreshPolicy]);

  // Pin the open branch to its current effective rules (copy-on-write override).
  const overridePolicy = useCallback(async () => {
    const id = currentRef.current;
    const cfg = policyRef.current;
    if (!id || !cfg) return;
    try {
      const { config, source } = await api.putSessionPolicy(id, cfg);
      setPolicy(config);
      setPolicySource(source);
    } catch {
      refreshPolicy();
    }
  }, [refreshPolicy]);

  // Drop the open branch's override so it inherits again.
  const clearPolicyOverride = useCallback(async () => {
    const id = currentRef.current;
    if (!id) return;
    try {
      await api.deleteSessionPolicy(id);
    } finally {
      refreshPolicy();
    }
  }, [refreshPolicy]);

  // Review payloads for the open session. Refetched on `changes.updated` (after a
  // workspace turn finishes, or after apply/revert).
  const refreshChanges = useCallback(async (id: string | null) => {
    if (!id) return setChanges([]);
    try {
      const { diffs } = await api.getChanges(id);
      setChanges(diffs);
    } catch {
      setChanges([]);
    }
  }, []);
  refreshChangesRef.current = refreshChanges;

  const open = useCallback(async (id: string) => {
    setCurrentId(id);
    setStreaming({});
    setChanges([]);
    setQueued([]); // staged messages belong to the session they were typed in
    // Opening a session is "seeing" it — the attention dot comes off.
    setSessions((prev) => prev.map((s) => (s.id === id && s.unseen ? { ...s, unseen: false } : s)));
    const { session, thread, usage } = await api.getSession(id);
    setSession(session);
    setThread(thread);
    setUsage(usage);
    refreshChanges(id);
    refreshPolicy(id); // the rail shows this branch's effective rules
    refreshNet(id); // …and this branch's egress feed, not other sessions' history
  }, [refreshChanges, refreshPolicy, refreshNet]);

  const applyChanges = useCallback((source: ChangeSource, paths: string[]) => {
    const id = currentRef.current;
    if (!id || paths.length === 0) return;
    // The `changes.updated` event triggers the refetch; no optimistic mutation needed.
    api.applyChanges(id, source, paths).catch(() => refreshChanges(id));
  }, [refreshChanges]);

  const revertChanges = useCallback(() => {
    const id = currentRef.current;
    if (!id) return;
    api.revertChanges(id).catch(() => refreshChanges(id));
  }, [refreshChanges]);

  // Fork/compact branch off the CURRENT session; the new session arrives via
  // session.created (→ heads) and we open it. A 400 (e.g. an inherited turn) surfaces
  // as a notice rather than a silent no-op.
  const fork = useCallback((atMessageId: string, editedText?: string) => {
    const id = currentRef.current;
    if (!id) return;
    const body = editedText !== undefined ? { atMessageId, editedText } : { atMessageId };
    api
      .fork(id, body)
      .then(readBranch)
      .then((s) => open(s.id))
      .catch((e: Error) => setNotice(e.message));
  }, [open]);

  const compact = useCallback((fromMessageId: string, toMessageId: string, sessionId?: string) => {
    const id = sessionId ?? currentRef.current;
    if (!id) return;
    api
      .compact(id, { fromMessageId, toMessageId })
      .then(readBranch)
      .then((s) => open(s.id))
      .catch((e: Error) => setNotice(e.message));
  }, [open]);

  const adopt = useCallback(() => {
    const id = currentRef.current;
    if (!id) return;
    api
      .adopt(id)
      // The changes.updated events refresh both rails; the notice confirms the squash.
      .then(({ message }) => setNotice(message))
      .catch((e: Error) => setNotice(`adopt failed: ${e.message}`));
  }, []);

  const dismissNotice = useCallback(() => setNotice(null), []);

  const newSession = useCallback(async (workspace?: string) => {
    // No title — the backend's title worker names the session from its first message.
    const s = await api.createSession({ ...(workspace ? { workspace } : {}) });
    // The session.created SSE event may have landed first — dedupe by id.
    setSessions((prev) => (prev.some((p) => p.id === s.id) ? prev : [s, ...prev]));
    await open(s.id);
    return s;
  }, [open]);

  // Sending while a turn runs STEERS by default: the message posts immediately and
  // the server yields the live turn at its next round boundary to answer it. With
  // queue=true it instead stages locally (visible, editable, removable); the flush
  // effect below sends staged messages once idle.
  const send = useCallback(async (text: string, queue = false) => {
    const id = currentRef.current;
    if (!id) return;
    if (busyRef.current && queue) {
      setQueued((q) => [...q, text]);
      return;
    }
    await api.postMessage(id, text);
  }, []);

  const removeQueued = useCallback((i: number) => {
    setQueued((q) => q.filter((_, idx) => idx !== i));
  }, []);
  const editQueued = useCallback((i: number, text: string) => {
    setQueued((q) => q.map((t, idx) => (idx === i ? text : t)));
  }, []);

  const interrupt = useCallback(() => {
    const id = currentRef.current;
    if (!id) return;
    api.interrupt(id).catch(() => {});
  }, []);

  // Archive drops the session from the sidebar (via session.archived); the open
  // thread stays on screen if it was current — no navigation surprise.
  const archive = useCallback((id: string) => {
    api.archiveSession(id).catch(() => {});
  }, []);

  const onEvent = useCallback((ev: BoughEvent) => {
    switch (ev.type) {
      case "session.created": {
        const s = ev.data as Session;
        setSessions((prev) => (prev.some((p) => p.id === s.id) ? prev : [s, ...prev]));
        break;
      }
      case "session.archived": {
        const { sessionId } = ev.data as { sessionId: string };
        setSessions((prev) => prev.filter((p) => p.id !== sessionId));
        break;
      }
      case "session.updated": {
        // e.g. the title worker renamed a session — patch it in place everywhere.
        // `busy`/`unseen` are client/list state (events don't carry them) — keep ours.
        const s = ev.data as Session;
        setSessions((prev) => prev.map((p) => (p.id === s.id ? { ...s, busy: p.busy, unseen: p.unseen } : p)));
        setSession((cur) => (cur && cur.id === s.id ? s : cur));
        break;
      }
      case "message.started": {
        const m = ev.data as Message;
        // A pending message opening means a turn is in flight — light the sidebar dot
        // for that session whether or not it's the open one.
        if (m.pending) {
          setSessions((prev) => prev.map((s) => (s.id === m.sessionId && !s.busy ? { ...s, busy: true } : s)));
        }
        if (m.sessionId !== currentRef.current) break;
        setThread((prev) => (prev.some((x) => x.id === m.id) ? prev : [...prev, m]));
        break;
      }
      case "message.delta": {
        const { messageId, delta } = ev.data as { messageId: string; delta: string };
        setStreaming((prev) => ({ ...prev, [messageId]: (prev[messageId] ?? "") + delta }));
        break;
      }
      case "message.part": {
        const { messageId, part } = ev.data as { messageId: string; part: Part };
        setThread((prev) =>
          prev.map((m) => (m.id === messageId ? { ...m, parts: [...m.parts, part] } : m))
        );
        // The finalized text part supersedes the streaming buffer.
        if (part.type === "text") {
          setStreaming((prev) => {
            const next = { ...prev };
            delete next[messageId];
            return next;
          });
        }
        break;
      }
      case "message.finished": {
        const { messageId } = ev.data as { messageId: string };
        if (ev.sessionId) {
          // Finished while another session is open → mark it "needs a look".
          const seen = ev.sessionId === currentRef.current;
          setSessions((prev) =>
            prev.map((s) =>
              s.id === ev.sessionId ? { ...s, busy: false, unseen: s.unseen || !seen } : s
            )
          );
        }
        setThread((prev) =>
          prev.map((m) => (m.id === messageId ? { ...m, pending: false } : m))
        );
        setStreaming((prev) => {
          const next = { ...prev };
          delete next[messageId];
          return next;
        });
        break;
      }
      case "turn.finished": {
        // How the turn ended (done/error/interrupted) — drives ✓/✗ status affixes.
        const { sessionId, status } = ev.data as {
          sessionId: string;
          status: Session["lastTurnStatus"];
        };
        setSessions((prev) =>
          prev.map((s) => (s.id === sessionId ? { ...s, lastTurnStatus: status } : s))
        );
        break;
      }
      case "net.request": {
        const r = ev.data as NetRequest;
        // Upsert by id: a held request is emitted twice (pending, then resolved), so the
        // row must update in place rather than duplicate. Only the open branch's rows
        // land in the feed; the pending card below stays global (see refreshNet).
        const openId = currentRef.current;
        if (!openId || r.sessionId === openId) {
          setNet((prev) => [r, ...prev.filter((x) => x.id !== r.id)].slice(0, 100));
        }
        setPendings((prev) => {
          if (r.verdict === "pending") {
            // enqueue (or refresh in place) — never displace the card being shown
            return prev.some((p) => p.id === r.id)
              ? prev.map((p) => (p.id === r.id ? r : p))
              : [...prev, r];
          }
          return prev.filter((p) => p.id !== r.id); // resolved/expired → next surfaces
        });
        break;
      }
      case "changes.updated": {
        const { sessionId } = ev.data as { sessionId: string };
        if (sessionId === currentRef.current) refreshChangesRef.current(sessionId);
        break;
      }
      case "usage.updated": {
        const u = ev.data as { sessionId: string; contextTokens: number; outputTokens: number };
        if (u.sessionId === currentRef.current) {
          setUsage({ contextTokens: u.contextTokens, outputTokens: u.outputTokens });
        }
        break;
      }
      default:
        // Unknown event types are ignored (rendered defensively elsewhere).
        break;
    }
  }, []);

  // Refetch everything the event stream would have told us about while it was down
  // (reconnect, or the tab waking from a background freeze). Streaming buffers are
  // dropped — their deltas are lost anyway; the refetched thread has the real parts.
  const resync = useCallback(async () => {
    reload().catch(() => {});
    refreshNetStatus().catch(() => {});
    refreshNet().catch(() => {});
    refreshPolicy().catch(() => {});
    const id = currentRef.current;
    if (!id) return;
    try {
      const { session, thread, usage } = await api.getSession(id);
      setSession(session);
      setThread(thread);
      setUsage(usage);
      setStreaming({});
      refreshChangesRef.current(id);
    } catch {
      // server unreachable — the next reconnect will resync again
    }
  }, [reload, refreshNetStatus, refreshNet, refreshPolicy]);

  const connected = useEvents(onEvent, resync);

  useEffect(() => {
    reload();
    refreshNetStatus();
    refreshNet();
    refreshPolicy();
  }, [reload, refreshNetStatus, refreshNet, refreshPolicy]);

  // A turn is running for the open session while any of its messages is pending.
  const busy = thread.some((m) => m.pending);
  busyRef.current = busy;

  // Flush staged messages once the turn finishes: post them all (the server queues
  // rapid posts into a single follow-up turn). Guard on currentId so a switch mid-turn
  // doesn't send them to the wrong session (open() already clears the queue).
  useEffect(() => {
    if (busy || queued.length === 0 || !currentId) return;
    const toSend = queued;
    setQueued([]);
    for (const text of toSend) api.postMessage(currentId, text).catch(() => {});
  }, [busy, queued, currentId]);

  return {
    sessions,
    currentId,
    session,
    thread,
    streaming,
    connected,
    busy,
    netStatus,
    net,
    pending,
    pendingCount: pendings.length,
    policy,
    policySource,
    changes,
    notice,
    usage,
    queued,
    removeQueued,
    editQueued,
    open,
    newSession,
    send,
    interrupt,
    archive,
    resolvePending,
    savePolicy,
    overridePolicy,
    clearPolicyOverride,
    refreshPolicy,
    reload,
    refreshNetStatus,
    applyChanges,
    revertChanges,
    fork,
    compact,
    adopt,
    dismissNotice,
  };
}
