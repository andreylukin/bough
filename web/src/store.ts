// App state + event reduction. Holds the session list, the open thread, per-message
// streaming buffers, and the network feed. Everything the UI renders derives from here.
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type NetConfig, type NetStatus, readBranch, type Usage } from "./api";
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
  // The editable rule set; null until loaded.
  policy: NetConfig | null;
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
  send: (text: string) => Promise<void>;
  interrupt: () => void;
  archive: (id: string) => void;
  resolvePending: (approve: boolean) => void;
  savePolicy: (cfg: NetConfig) => Promise<void>;
  reload: () => Promise<void>;
  refreshNetStatus: () => Promise<void>;
  applyChanges: (source: ChangeSource, paths: string[]) => void;
  revertChanges: () => void;
  fork: (atMessageId: string, editedText?: string) => void;
  // sessionId defaults to the open session (conversation compact); the map passes an
  // explicit head so a span on any lane compacts the right session.
  compact: (fromMessageId: string, toMessageId: string, sessionId?: string) => void;
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
    proxyUrl: "",
    caPath: "",
  });
  const [net, setNet] = useState<NetRequest[]>([]);
  const [pending, setPending] = useState<NetRequest | null>(null);
  const [policy, setPolicy] = useState<NetConfig | null>(null);
  const [changes, setChanges] = useState<WireDiff[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [usage, setUsage] = useState<Usage>({ contextTokens: 0, outputTokens: 0 });
  const [queued, setQueued] = useState<string[]>([]);

  // currentId in a ref so the event handler (stable) can filter without re-subscribing.
  const currentRef = useRef<string | null>(null);
  currentRef.current = currentId;

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
  // Global (no session filter) so the feed shows every gated request; the newest
  // pending row, if any, surfaces as the hold-and-ask card.
  const refreshNet = useCallback(async () => {
    const rows = await api.netRequests();
    setNet(rows);
    setPending(rows.find((r) => r.verdict === "pending") ?? null);
  }, []);

  const refreshPolicy = useCallback(async () => {
    try {
      setPolicy(await api.getPolicy());
    } catch {
      setPolicy(null);
    }
  }, []);

  const resolvePending = useCallback((approve: boolean) => {
    const req = pendingRef.current;
    // Clear the card optimistically; the gate re-emits the row with its final verdict,
    // which the `net.request` handler reconciles by id.
    setPending(null);
    if (!req) return;
    (approve ? api.allowRequest(req.id) : api.denyRequest(req.id)).catch(() => {
      // The hold may have already resolved/expired server-side (404). Re-sync so the
      // rail reflects the true state rather than a stale card.
      refreshNet();
    });
  }, [refreshNet]);

  // Persist a new rule set; PUT hot-swaps the live gate. Optimistic, then reconcile.
  const savePolicy = useCallback(async (cfg: NetConfig) => {
    setPolicy(cfg);
    try {
      setPolicy(await api.putPolicy(cfg));
    } catch {
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
  }, [refreshChanges]);

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

  const dismissNotice = useCallback(() => setNotice(null), []);

  const newSession = useCallback(async (workspace?: string) => {
    // No title — the backend's title worker names the session from its first message.
    const s = await api.createSession({ ...(workspace ? { workspace } : {}) });
    // The session.created SSE event may have landed first — dedupe by id.
    setSessions((prev) => (prev.some((p) => p.id === s.id) ? prev : [s, ...prev]));
    await open(s.id);
    return s;
  }, [open]);

  // Sending while a turn runs stages the message locally (visible, editable, removable)
  // instead of posting it; the flush effect below sends staged messages once idle.
  const send = useCallback(async (text: string) => {
    const id = currentRef.current;
    if (!id) return;
    if (busyRef.current) {
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
      case "net.request": {
        const r = ev.data as NetRequest;
        // Upsert by id: a held request is emitted twice (pending, then resolved), so the
        // row must update in place rather than duplicate.
        setNet((prev) => [r, ...prev.filter((x) => x.id !== r.id)].slice(0, 100));
        setPending((cur) => {
          if (r.verdict === "pending") return r;
          if (cur && cur.id === r.id) return null; // this hold just resolved
          return cur;
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
    policy,
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
    reload,
    refreshNetStatus,
    applyChanges,
    revertChanges,
    fork,
    compact,
    dismissNotice,
  };
}
