// App state + event reduction — port of web/src/store.ts trimmed to the TUI's P1
// scope (sessions, open thread, streaming buffers, net holds, queued messages).
// Policy/changes/usage/fork land with their panels in later phases.
import { useCallback, useEffect, useRef, useState } from "react";
import type { BoughEvent, Message, NetRequest, Part, Session } from "../schema/parts.ts";
import { api, type Usage, USAGE_ZERO, type WireDiff } from "./api.ts";
import { useEvents } from "./events.ts";

// Client-side decorations the wire Session doesn't carry.
// Net-feed rows kept in memory for the panel.
const FEED_CAP = 50;

export type TuiSession = Session & {
  busy?: boolean;
  unseen?: boolean;
  lastTurnStatus?: "done" | "error" | "interrupted" | "orphaned";
};

export interface Store {
  sessions: TuiSession[];
  currentId: string | null;
  session: TuiSession | null;
  thread: Message[];
  // messageId -> live text accumulated from message.delta, shown until the finalized
  // text part lands (then cleared to avoid double-rendering).
  streaming: Record<string, string>;
  connected: boolean;
  busy: boolean;
  // Oldest pending net hold (the card shows one at a time) + how many wait in total.
  pending: NetRequest | null;
  pendingCount: number;
  // Messages typed while a turn was running — staged locally, flushed once idle.
  queued: string[];
  notice: string | null;
  // Review payloads for the open session; refetched on changes.updated.
  changes: WireDiff[];
  // Token usage for the open session (status-bar meter); zeroed on session switch.
  usage: Usage;
  // Recent gated requests, newest first (net panel feed) — all verdicts, not just holds.
  feed: NetRequest[];
  open: (id: string) => Promise<void>;
  newSession: (workspace?: string) => Promise<Session>;
  /** Post a message. While busy: posts immediately (steer) unless queue=true.
   * `id` overrides the current session (used right after a draft's session is
   * created, before the state round-trip lands). */
  send: (text: string, queue?: boolean, id?: string) => Promise<void>;
  interrupt: () => void;
  archive: (id: string) => void;
  deprecate: (id: string, on: boolean) => void;
  resolvePending: (approve: boolean, scope?: "once" | "session") => void;
  // Branch off the current session at a message (optionally cut mid-message at a
  // tool run via atPart); opens the new branch (or notices).
  fork: (atMessageId: string, atPart?: number) => Promise<Session | null>;
  // Compact the current session's own turns onto a summary branch; opens it.
  compact: () => Promise<Session | null>;
  compactPicks: (msgIds: string[]) => Promise<Session | null>;
  extractPicks: (msgIds: string[]) => Promise<Session | null>;
  deleteRange: (rangeIds: string[]) => Promise<Session | null>;
  moveRange: (targetId: string, rangeIds: string[]) => Promise<Session | null>;
  applyChanges: (source: WireDiff["source"], paths: string[]) => void;
  revertChanges: () => void;
  dismissNotice: () => void;
  /** Show a transient notice (auto-clears) — feedback for actions that would
   * otherwise be silent (fork created, range deleted, key not applicable). */
  notify: (msg: string) => void;
}

export function useStore(initialSessions: Session[]): Store {
  const [sessions, setSessions] = useState<TuiSession[]>(initialSessions);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [session, setSession] = useState<TuiSession | null>(null);
  const [thread, setThread] = useState<Message[]>([]);
  const [streaming, setStreaming] = useState<Record<string, string>>({});
  // ALL holds awaiting approval, oldest first.
  const [pendings, setPendings] = useState<NetRequest[]>([]);
  const pending = pendings[0] ?? null;
  const [queued, setQueued] = useState<string[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [changes, setChanges] = useState<WireDiff[]>([]);
  const [usage, setUsage] = useState<Usage>(USAGE_ZERO);
  const [feed, setFeed] = useState<NetRequest[]>([]);

  // currentId in a ref so the stable event handler can filter without re-subscribing.
  const currentRef = useRef<string | null>(null);
  currentRef.current = currentId;
  const threadRef = useRef<Message[]>([]);
  threadRef.current = thread;
  const busyRef = useRef(false);
  const pendingRef = useRef<NetRequest | null>(null);
  pendingRef.current = pending;

  const reload = useCallback(async () => {
    const s = await api.listSessions();
    // busy/unseen are client-side memory — carry them across the server refetch.
    setSessions((prev) =>
      s.map((n) => {
        const old = prev.find((p) => p.id === n.id);
        return { ...n, busy: old?.busy, unseen: old?.unseen, lastTurnStatus: old?.lastTurnStatus };
      })
    );
  }, []);

  const refreshPendings = useCallback(async () => {
    const all = await api.netRequests();
    setFeed(all.slice(0, FEED_CAP)); // server returns newest first
    setPendings(all.filter((r) => r.verdict === "pending").reverse()); // oldest first
  }, []);

  const resolvePending = useCallback((approve: boolean, scope: "once" | "session" = "once") => {
    const req = pendingRef.current;
    // Drop this card optimistically — the next parked hold surfaces right away; the
    // gate re-emits the row with its final verdict, reconciled by id.
    setPendings((prev) => prev.filter((p) => p.id !== req?.id));
    if (!req) return;
    (approve ? api.allowRequest(req.id, scope) : api.denyRequest(req.id)).catch(() => {
      // The hold may have already resolved/expired server-side — re-sync.
      refreshPendings().catch(() => {});
    });
  }, [refreshPendings]);

  const refreshChanges = useCallback(async (id: string | null) => {
    if (!id) return setChanges([]);
    try {
      const { diffs } = await api.getChanges(id);
      setChanges(diffs);
    } catch {
      setChanges([]);
    }
  }, []);
  const refreshChangesRef = useRef(refreshChanges);
  refreshChangesRef.current = refreshChanges;

  const open = useCallback(async (id: string) => {
    setCurrentId(id);
    setStreaming({});
    setQueued([]); // staged messages belong to the session they were typed in
    setChanges([]);
    setUsage(USAGE_ZERO);
    setSessions((prev) => prev.map((s) => (s.id === id && s.unseen ? { ...s, unseen: false } : s)));
    const { session, thread, usage } = await api.getSession(id);
    setSession(session);
    setThread(thread);
    setUsage(usage);
    refreshChanges(id);
  }, [refreshChanges]);

  const newSession = useCallback(async (workspace?: string) => {
    // No title — the backend's title worker names the session from its first message.
    const s = await api.createSession(workspace ? { workspace } : {});
    setSessions((prev) => (prev.some((p) => p.id === s.id) ? prev : [s, ...prev]));
    await open(s.id);
    return s;
  }, [open]);

  // Transient toast: unlike error notices (which persist until replaced), these
  // self-dismiss — the message confirms an action, it shouldn't linger as state.
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const notify = useCallback((msg: string) => {
    setNotice(msg);
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    noticeTimer.current = setTimeout(() => {
      noticeTimer.current = null;
      setNotice((n) => (n === msg ? null : n));
    }, 5000);
  }, []);

  const fork = useCallback(async (atMessageId: string, atPart?: number) => {
    const id = currentRef.current;
    if (!id) return null;
    try {
      return await api.fork(id, atPart === undefined ? { atMessageId } : { atMessageId, atPart });
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
      return null;
    }
  }, []);

  // Compact all of the session's OWN turns (inherited ancestor turns are skipped —
  // the server 400s picks that reach into ancestor history).
  const compact = useCallback(async () => {
    const id = currentRef.current;
    if (!id) return null;
    const picks = threadRef.current
      .filter((m) => m.sessionId === id)
      .map((m) => ({ messageId: m.id }));
    if (picks.length === 0) {
      setNotice("nothing to compact — no own turns");
      return null;
    }
    try {
      return await api.compact(id, picks);
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
      return null;
    }
  }, []);

  // Range ops on a hand-picked set of turn message ids (only this session's OWN
  // messages qualify; the server 400s picks that reach into ancestor history).
  const rangeOp = useCallback(
    async (
      msgIds: string[],
      run: (id: string, picks: { messageId: string }[]) => Promise<Session>,
    ) => {
      const id = currentRef.current;
      if (!id) return null;
      const own = new Set(threadRef.current.filter((m) => m.sessionId === id).map((m) => m.id));
      const picks = msgIds.filter((mid) => own.has(mid)).map((messageId) => ({ messageId }));
      if (picks.length === 0) {
        setNotice("select turns from this conversation (not inherited history)");
        return null;
      }
      try {
        return await run(id, picks);
      } catch (e) {
        setNotice(e instanceof Error ? e.message : String(e));
        return null;
      }
    },
    [],
  );
  const compactPicks = useCallback((ids: string[]) => rangeOp(ids, api.compact), [rangeOp]);
  const extractPicks = useCallback((ids: string[]) => rangeOp(ids, api.extract), [rangeOp]);

  // Delete a section: branch the conversation WITHOUT the picked turns (extract the
  // complement), then archive the original — recoverable until the 30-day purge.
  const deleteRange = useCallback(async (rangeIds: string[]) => {
    const id = currentRef.current;
    if (!id) return null;
    const cut = new Set(rangeIds);
    const keep = threadRef.current
      .filter((m) => m.sessionId === id && !cut.has(m.id))
      .map((m) => ({ messageId: m.id }));
    if (keep.length === 0) {
      setNotice("that's the whole conversation — archive it from the sessions tab instead");
      return null;
    }
    try {
      // replaceSource: the new session takes the original's title + lineage spot.
      const s = await api.extract(id, keep, true);
      await api.archiveSession(id);
      notify("turns deleted — the original conversation is archived (recoverable)");
      return s;
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
      return null;
    }
  }, [notify]);

  // Move a section onto an existing branch: append the picked turns to the target.
  const moveRange = useCallback(async (targetId: string, rangeIds: string[]) => {
    const id = currentRef.current;
    if (!id) return null;
    const own = new Set(threadRef.current.filter((m) => m.sessionId === id).map((m) => m.id));
    const picks = rangeIds.filter((mid) => own.has(mid)).map((messageId) => ({ messageId }));
    if (picks.length === 0) {
      setNotice("select turns from this conversation (not inherited history)");
      return null;
    }
    try {
      return await api.moveInto(targetId, id, picks);
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
      return null;
    }
  }, []);

  const applyChanges = useCallback((source: WireDiff["source"], paths: string[]) => {
    const id = currentRef.current;
    if (!id || paths.length === 0) return;
    // The changes.updated event triggers the refetch; no optimistic mutation needed.
    api.applyChanges(id, source, paths).catch(() => refreshChangesRef.current(id));
  }, []);

  const revertChanges = useCallback(() => {
    const id = currentRef.current;
    if (!id) return;
    api.revertChanges(id).catch((e) => setNotice(e instanceof Error ? e.message : String(e)));
  }, []);

  const send = useCallback(async (text: string, queue = false, idArg?: string) => {
    const id = idArg ?? currentRef.current;
    if (!id) return;
    if (busyRef.current && queue) {
      setQueued((q) => [...q, text]);
      return;
    }
    await api.postMessage(id, text);
  }, []);

  const interrupt = useCallback(() => {
    const id = currentRef.current;
    if (!id) return;
    api.interrupt(id).catch(() => {});
  }, []);

  const archive = useCallback((id: string) => {
    api.archiveSession(id).catch(() => {});
  }, []);

  const deprecate = useCallback((id: string, on: boolean) => {
    api.deprecateSession(id, on).catch(() => {}); // session.updated event reflects it
  }, []);

  const dismissNotice = useCallback(() => setNotice(null), []);

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
        const s = ev.data as Session;
        setSessions((prev) =>
          prev.map((p) =>
            p.id === s.id
              ? { ...s, busy: p.busy, unseen: p.unseen, lastTurnStatus: p.lastTurnStatus }
              : p
          )
        );
        setSession((cur) => (cur && cur.id === s.id ? { ...cur, ...s } : cur));
        break;
      }
      case "message.started": {
        const m = ev.data as Message;
        if (m.pending) {
          setSessions((prev) =>
            prev.map((s) => (s.id === m.sessionId && !s.busy ? { ...s, busy: true } : s))
          );
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
          const seen = ev.sessionId === currentRef.current;
          setSessions((prev) =>
            prev.map((s) =>
              s.id === ev.sessionId ? { ...s, busy: false, unseen: s.unseen || !seen } : s
            )
          );
        }
        setThread((prev) => prev.map((m) => (m.id === messageId ? { ...m, pending: false } : m)));
        setStreaming((prev) => {
          const next = { ...prev };
          delete next[messageId];
          return next;
        });
        break;
      }
      case "turn.finished": {
        const { sessionId, status } = ev.data as {
          sessionId: string;
          status: TuiSession["lastTurnStatus"];
        };
        setSessions((prev) =>
          prev.map((s) => (s.id === sessionId ? { ...s, lastTurnStatus: status } : s))
        );
        break;
      }
      case "changes.updated": {
        const { sessionId } = ev.data as { sessionId: string };
        if (sessionId === currentRef.current) refreshChangesRef.current(sessionId);
        break;
      }
      case "usage.updated": {
        const u = ev.data as { sessionId: string } & Usage;
        if (u.sessionId === currentRef.current) setUsage(u);
        break;
      }
      case "net.request": {
        const r = ev.data as NetRequest;
        // Feed: upsert by id (verdict flips re-emit the row), newest first.
        setFeed((prev) => {
          const rest = prev.filter((p) => p.id !== r.id);
          return [r, ...rest].slice(0, FEED_CAP);
        });
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
      default:
        break;
    }
  }, []);

  // Refetch everything the stream would have told us about while it was down. Streaming
  // buffers are dropped — their deltas are lost anyway; the refetched thread has the
  // real parts (and the server marks orphaned turns, so stale `pending` clears too).
  const resync = useCallback(async () => {
    reload().catch(() => {});
    refreshPendings().catch(() => {});
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
  }, [reload, refreshPendings]);

  const connected = useEvents(onEvent, resync);

  useEffect(() => {
    refreshPendings().catch(() => {});
  }, [refreshPendings]);

  const busy = thread.some((m) => m.pending);
  busyRef.current = busy;

  // Flush staged messages once the turn finishes (the server queues rapid posts into
  // a single follow-up turn). Guard on currentId so a switch mid-turn doesn't send
  // them to the wrong session (open() already clears the queue).
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
    pending,
    pendingCount: pendings.length,
    queued,
    notice,
    changes,
    usage,
    feed,
    open,
    newSession,
    send,
    interrupt,
    archive,
    deprecate,
    resolvePending,
    fork,
    compact,
    compactPicks,
    extractPicks,
    deleteRange,
    moveRange,
    applyChanges,
    revertChanges,
    dismissNotice,
    notify,
  };
}
