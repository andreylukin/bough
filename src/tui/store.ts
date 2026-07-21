// App state + event reduction — originally a port of the retired web UI's store, trimmed to the TUI's P1
// scope (sessions, open thread, streaming buffers, net holds, queued messages).
// Policy/changes/usage/fork land with their panels in later phases.
import { useCallback, useEffect, useRef, useState } from "react";
import type {
  AskQuestion,
  BoughEvent,
  Message,
  NetRequest,
  Part,
  Session,
} from "../schema/parts.ts";
import { api, type Usage, USAGE_ZERO, type WireDiff } from "./api.ts";
import { useEvents } from "./events.ts";
import { notifyDesktop } from "./term.ts";

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
  // toolCallId -> console lines streamed live from the running program (tool.log
  // events). Cleared when the message finishes; the final tool_result part then
  // carries the same lines in its output.
  toolLogs: Record<string, string[]>;
  connected: boolean;
  busy: boolean;
  // Oldest pending net hold (the card shows one at a time) + how many wait in total.
  pending: NetRequest | null;
  pendingCount: number;
  // Oldest pending ask() question (net holds take precedence in the UI) + total.
  ask: AskQuestion | null;
  askCount: number;
  // Messages typed while a turn was running — staged locally, flushed once idle.
  queued: string[];
  notice: string | null;
  // Local-worker blurb of what the running turn's program is doing ("running the
  // test suite") — shown next to the working spinner; null when idle/unknown.
  activity: string | null;
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
  /** Answer the surfaced ask() question (chosen option or typed free text). */
  answerAsk: (answer: string) => void;
  /** Decline it — the program's ask() rejects with a "user declined" error. */
  declineAsk: () => void;
  // Branch off the current session at a message (optionally cut mid-message at a
  // tool run via atPart); opens the new branch (or notices). `id` overrides the
  // current session (the conversation tree re-roots at the spawner inside a
  // subagent, so its rewind/branch ops act on the parent).
  fork: (
    atMessageId: string,
    atPart?: number,
    exclusive?: boolean,
    id?: string,
  ) => Promise<Session | null>;
  // Compact the current session's own turns onto a summary branch; opens it.
  compact: () => Promise<Session | null>;
  compactPicks: (msgIds: string[]) => Promise<Session | null>;
  extractPicks: (msgIds: string[]) => Promise<Session | null>;
  /** Draft a goal-focused opening prompt from this thread onto a fresh conversation. */
  handoff: (goal: string) => Promise<Session | null>;
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
  const [toolLogs, setToolLogs] = useState<Record<string, string[]>>({});
  // ALL holds awaiting approval, oldest first.
  const [pendings, setPendings] = useState<NetRequest[]>([]);
  const pending = pendings[0] ?? null;
  // ALL ask() questions awaiting an answer, oldest first.
  const [asks, setAsks] = useState<AskQuestion[]>([]);
  const ask = asks[0] ?? null;
  const [queued, setQueued] = useState<string[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [activity, setActivity] = useState<string | null>(null);
  const [changes, setChanges] = useState<WireDiff[]>([]);
  const [usage, setUsage] = useState<Usage>(USAGE_ZERO);
  const [feed, setFeed] = useState<NetRequest[]>([]);

  // currentId in a ref so the stable event handler can filter without re-subscribing.
  const currentRef = useRef<string | null>(null);
  currentRef.current = currentId;
  const threadRef = useRef<Message[]>([]);
  threadRef.current = thread;
  // For desktop notifications: the stable event handler needs session titles.
  const sessionsRef = useRef<TuiSession[]>([]);
  sessionsRef.current = sessions;
  const busyRef = useRef(false);
  const pendingRef = useRef<NetRequest | null>(null);
  pendingRef.current = pending;
  const askRef = useRef<AskQuestion | null>(null);
  askRef.current = ask;

  const reload = useCallback(async () => {
    const s = await api.listSessions();
    // busy/unseen are client-side memory — carry them across the server refetch.
    // lastTurnStatus is server-authoritative (the last finished turn's status), so
    // prefer the fresh value and fall back to client memory only when it's absent —
    // otherwise a failed/interrupted subagent shows no status until a live event.
    setSessions((prev) =>
      s.map((n) => {
        const old = prev.find((p) => p.id === n.id);
        // The server augments the row with a runtime lastTurnStatus (not in the
        // persisted schema type) — read it via a cast and prefer it over memory.
        const serverStatus = (n as { lastTurnStatus?: TuiSession["lastTurnStatus"] })
          .lastTurnStatus;
        return {
          ...n,
          busy: old?.busy,
          unseen: old?.unseen,
          lastTurnStatus: serverStatus ?? old?.lastTurnStatus,
        };
      })
    );
  }, []);

  const refreshPendings = useCallback(async () => {
    const all = await api.netRequests();
    setFeed(all.slice(0, FEED_CAP)); // server returns newest first
    setPendings(all.filter((r) => r.verdict === "pending").reverse()); // oldest first
  }, []);

  // Rebuild the ask() hold card after (re)attach — the server returns pending
  // questions oldest first, so a reconnecting client sees the same hold.
  const refreshAsks = useCallback(async () => {
    setAsks(await api.questions());
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

  // Same optimistic-drop shape as resolvePending: the next question surfaces right
  // away; the settle re-emits the row (final status), reconciled by id.
  const settleAsk = useCallback((run: (q: AskQuestion) => Promise<unknown>) => {
    const q = askRef.current;
    if (!q) return;
    setAsks((prev) => prev.filter((p) => p.id !== q.id));
    run(q).catch(() => {
      // Already settled/expired server-side — re-sync.
      refreshAsks().catch(() => {});
    });
  }, [refreshAsks]);
  const answerAsk = useCallback(
    (answer: string) => settleAsk((q) => api.answerQuestion(q.sessionId, q.id, answer)),
    [settleAsk],
  );
  const declineAsk = useCallback(
    () => settleAsk((q) => api.declineQuestion(q.sessionId, q.id)),
    [settleAsk],
  );

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
    setActivity(null); // a blurb describes the session it was born in
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

  const fork = useCallback(async (
    atMessageId: string,
    atPart?: number,
    exclusive?: boolean,
    idArg?: string,
  ) => {
    const id = idArg ?? currentRef.current;
    if (!id) return null;
    try {
      return await api.fork(id, {
        atMessageId,
        ...(atPart === undefined ? {} : { atPart }),
        ...(exclusive ? { exclusive: true } : {}),
      });
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

  // Handoff (focused threads instead of compaction): the server drafts a
  // self-contained opening prompt from this thread toward `goal` and attaches it
  // to a fresh conversation as session.draft — the composer prefills from it and
  // the first send consumes it.
  const handoff = useCallback(async (goal: string) => {
    const id = currentRef.current;
    if (!id) return null;
    try {
      return await api.handoff(id, goal);
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
      return null;
    }
  }, []);

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
    api.applyChanges(id, source, paths).then((r) => {
      // Say where the files actually went — silence here reads as a failed no-op.
      const n = r.applied.length;
      if (n === 0) return notify("nothing to apply");
      const files = `${n} file${n === 1 ? "" : "s"}`;
      if (r.origin) {
        notify(`✓ ${files} → ${r.origin}${r.sealed && r.branch ? ` · sealed as ${r.branch}` : ""}`);
      } else {
        notify(`✓ accepted ${files}${r.branch ? ` · kept on ${r.branch}` : ""}`);
      }
    }, (e) => {
      setNotice(e instanceof Error ? e.message : String(e));
      refreshChangesRef.current(id);
    });
  }, [notify]);

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
      case "message.retry": {
        // The round is being re-attempted and will re-stream from the top —
        // drop the partial streamed text so it doesn't double up.
        const { messageId } = ev.data as { messageId: string };
        setStreaming((prev) => {
          if (prev[messageId] === undefined) return prev;
          const next = { ...prev };
          delete next[messageId];
          return next;
        });
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
      case "tool.log": {
        // A console line from the running program — accumulate under its tool
        // call; the group renders these while the call has no result yet.
        const { callId, line } = ev.data as { messageId: string; callId: string; line: string };
        setToolLogs((prev) => ({ ...prev, [callId]: [...(prev[callId] ?? []), line] }));
        break;
      }
      case "session.activity": {
        const { text } = ev.data as { text: string };
        if (ev.sessionId === currentRef.current) setActivity(text);
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
          if (ev.sessionId === currentRef.current) setActivity(null);
          // Desktop banner when a turn lands while the terminal is unfocused
          // (notifyDesktop self-gates on focus). Subagent turns finish inside a
          // parent's still-running turn — a banner there would be noise.
          const fin = sessionsRef.current.find((s) => s.id === ev.sessionId);
          if (fin?.busy && fin.kind !== "subagent") {
            notifyDesktop(`${fin.title || "bough"} — turn finished`);
          }
        }
        setThread((prev) => prev.map((m) => (m.id === messageId ? { ...m, pending: false } : m)));
        setStreaming((prev) => {
          const next = { ...prev };
          delete next[messageId];
          return next;
        });
        // The turn is over — drop live log buffers (their lines now live in the
        // finalized tool_result outputs).
        setToolLogs((prev) => {
          const done = new Set(
            threadRef.current
              .filter((m) => m.id === messageId)
              .flatMap((m) => m.parts)
              .filter((p) => p.type === "tool_call")
              .map((p) => p.id),
          );
          const next = Object.fromEntries(
            Object.entries(prev).filter(([id]) => !done.has(id)),
          );
          return Object.keys(next).length === Object.keys(prev).length ? prev : next;
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
            // A NEW hold (not a refresh) wants eyes on it — desktop banner if
            // the terminal is unfocused (notifyDesktop self-gates on focus).
            if (!prev.some((p) => p.id === r.id)) {
              notifyDesktop(`bough — approval needed: ${r.host}`);
              return [...prev, r];
            }
            return prev.map((p) => (p.id === r.id ? r : p));
          }
          return prev.filter((p) => p.id !== r.id); // resolved/expired → next surfaces
        });
        break;
      }
      case "ask.question": {
        const q = ev.data as AskQuestion;
        setAsks((prev) => {
          if (q.status === "pending") {
            // A NEW question wants eyes on it — banner if the terminal is
            // unfocused (notifyDesktop self-gates on focus), like a net hold.
            if (!prev.some((p) => p.id === q.id)) {
              notifyDesktop(`bough — question: ${q.question}`);
              return [...prev, q];
            }
            return prev.map((p) => (p.id === q.id ? q : p));
          }
          return prev.filter((p) => p.id !== q.id); // settled → next surfaces
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
    refreshAsks().catch(() => {});
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
  }, [reload, refreshPendings, refreshAsks]);

  const connected = useEvents(onEvent, resync);

  useEffect(() => {
    refreshPendings().catch(() => {});
    refreshAsks().catch(() => {});
  }, [refreshPendings, refreshAsks]);

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
    toolLogs,
    connected,
    busy,
    pending,
    pendingCount: pendings.length,
    ask,
    askCount: asks.length,
    queued,
    notice,
    activity,
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
    answerAsk,
    declineAsk,
    fork,
    compact,
    compactPicks,
    extractPicks,
    handoff,
    deleteRange,
    moveRange,
    applyChanges,
    revertChanges,
    dismissNotice,
    notify,
  };
}
