/**
 * Input handling: keys are DATA, and the help overlay is generated from that data.
 *
 * THE INVARIANT THIS HOLDS: **there is exactly one description of what a key does,
 * and it is the thing that makes the key do it.** `BINDINGS` is a flat table of
 * `(mode, chord, command)` rows carrying their own help text; `lookup()` resolves a
 * keypress against it and `helpSections()` renders it. The old tree kept the two
 * apart — a 109-line `keys.ts` of prose next to a 3,618-line `App.tsx` of
 * `if (key.ctrl && ch === "f")` — so the overlay documented keys that had been
 * renamed and stayed silent about ones that had been added. Here that cannot
 * happen: a row that is not bound cannot be documented, and a chord that is bound
 * twice in the same mode is a test failure rather than a dead binding nobody
 * notices.
 *
 * SECOND INVARIANT — **resolution is pure and needs no terminal.** `chordOf`
 * canonicalizes ink's `(input, key)` pair into a string; `lookup` picks the first
 * row whose guards hold against a plain context object. Both are functions of
 * data, so `keys.test.ts` asserts the whole keymap with no TTY, no renderer and no
 * server (task AC; plan §7).
 *
 * THIRD — **the same chord may mean two things, and the guard says which.** `^f`
 * opens the tree on an empty composer and moves the cursor forward when there is
 * text; `↑` walks history on one line and moves the cursor on several. That is not
 * an accident to be tidied away, it is how a composer and a pager share a
 * keyboard. The guards are explicit fields rather than an ordering trick, and the
 * duplicate test knows the difference: two rows with the same chord AND the same
 * guards is a bug, an unguarded row placed ahead of a guarded one is a bug, and
 * two rows with complementary guards is the design.
 *
 * FOURTH — **the panel's tab list is part of the keymap, and lives here.** Spec §15
 * gives bough exactly one non-chat surface with direct-jump keys, so `TABS` is
 * declared in this module and `Command` derives its `tab.*` members from it. A tab
 * therefore cannot exist without a chord, cannot be documented without being bound,
 * and cannot be reached by a second route: `Panel.tsx` imports this table and
 * re-exports it, and this module imports nothing from `components/`.
 *
 * FIFTH — **a tab-local key says so in the table, not in its prose.** A bare letter
 * in the panel means whatever the open tab says it means, and for a while the table
 * expressed that as "(workflows)" in a description — which the dispatcher could not
 * read, so `p` steered a run from the sessions list. `Binding.tab` is that scope as
 * data, matched by `lookup` against `KeyContext.tab`, which is what lets `x` be
 * `wf.stop` in one tab and `changes.revert` in another with neither shadowing the
 * other and `deadBindings` still able to tell a collision from a design.
 *
 * SIXTH — **`key.super` is only believable under the kitty keyboard protocol.**
 * Without it a terminal sends Cmd+←/→ as `CSI 1;9 C/D` and ink leaks bit 3 of the
 * modifier field into the meta flag, so those sequences are intercepted in
 * `mouse.ts` and delivered as nav-key events instead. `term.ts` decides which path
 * is live; this module binds both, which is why `super+left` and the intercepted
 * `cmdHome` land on the same command.
 *
 * Line editing lives here too, as pure `LineState → LineState` functions. The
 * composer's cursor arithmetic is the part users notice when it is a character off
 * on a wrapped paste, and it has no business being inside a React component.
 */
import stripAnsi from "strip-ansi";
import { wordLeft, wordRight } from "./format.ts";

// ---------------------------------------------------------------------------
// Modes and commands
// ---------------------------------------------------------------------------

/**
 * Which surface has the keyboard. Not a view stack: a mode is answered by exactly
 * one binding set, so a chord can never be handled twice on its way down.
 *
 * There is ONE non-chat surface — `panel` — because spec §15 says there is one:
 * sessions, tree, changes, workflows, model, MCP, skills and theme are TABS of it,
 * not modes beside it. The earlier draft of this table had a `tree` mode and a
 * `workflows` mode, which is the shape the 3,618-line `App.tsx` grew out of: every
 * surface with its own mode, its own way in, and its own escape.
 */
/**
 * `job` is the surface one background shell's output gets when you open it from
 * the rail — its own mode and not a panel tab, because it is addressed to ONE job
 * and is entered from the row that names it, the way a workflow's drill-in is.
 */
export type UiMode = "chat" | "rail" | "ask" | "panel" | "help" | "job";

export type Command =
  // -- global ---------------------------------------------------------------
  /** First ^c: show the quit hint. A single ^c must never unmount ink under it. */
  | "quit.arm"
  | "quit"
  | "help.open"
  | "help.close"
  // -- the one tabbed panel (spec §15) --------------------------------------
  | "panel.toggle"
  | "panel.close"
  | "panel.next"
  | "panel.prev"
  /** The active tab's affirmative: open a session, grant a server, keep a theme. */
  | "panel.confirm"
  /** Branch at the cursor AND carry a summary of the abandoned path (pi's /tree). */
  | "panel.confirmSummarize"
  /**
   * Jump straight to row 1-9 of the active tab AND affirm it (spec §3: options are
   * addressable by digit, not only arrowable). The digit is read off the keypress
   * by the dispatcher, exactly as `ask.pick` already does.
   */
  | "panel.pick"
  // -- the panel's type-to-filter (one modal buffer, every list tab) --------
  /** `/` — hand the keyboard to the filter buffer. Guarded OFF while it has it. */
  | "panel.filter"
  | "panel.filterBack"
  /** Clear the buffer and give the keyboard back. The panel stays open. */
  | "panel.filterExit"
  /** One per tab, derived from `TABS` so a tab cannot exist without a chord. */
  | TabCommand
  /**
   * Open the tree with the cursor on the open conversation's last turn — `esc esc`.
   * A separate command from `tab.tree` because the landing row is the whole point.
   */
  | "tree.rewind"
  | "tree.extract"
  | "tree.moveInto"
  | "mcp.restart"
  /**
   * Start a fresh root conversation.
   *
   * A session was only ever created IMPLICITLY, by sending the first message with
   * none open — so once you were in a conversation there was no way to begin
   * another without quitting the TUI and running `bough` again. The tree can open
   * an existing conversation and fork a turn; it could not start one.
   */
  | "session.new"
  | "session.compact"
  /** Show the recurring runs — the only window the TUI has onto them. */
  | "schedules.show"
  /** List the saved workflows — the tab promises they can be run by name. */
  | "saved.show"
  /** List this conversation's published artifacts and their URLs. */
  | "artifacts.show"
  // -- composing ------------------------------------------------------------
  | "send"
  | "send.queue"
  | "newline"
  | "draft.clear"
  | "cancel"
  /** Stop the running turn (spec §5). Distinct from `cancel`, which dismisses a notice. */
  | "turn.interrupt"
  | "history.prev"
  | "history.next"
  // -- the @/ completion popup (guarded on `completing`) -------------------
  | "complete.accept"
  /** ⇥ with no completion popup open: take the cheap tier's suggested next message. */
  | "ghost.accept"
  | "complete.prev"
  | "complete.next"
  | "complete.dismiss"
  // -- reading --------------------------------------------------------------
  | "fold.all"
  | "scroll.up"
  | "scroll.down"
  /** A whole screen, not a step. The `?` overlay is 50 rows and ↑↓ scan it. */
  | "scroll.pageUp"
  | "scroll.pageDown"
  // -- editing the line -----------------------------------------------------
  | "cursor.left"
  | "cursor.right"
  | "cursor.home"
  | "cursor.end"
  | "cursor.wordLeft"
  | "cursor.wordRight"
  | "cursor.up"
  | "cursor.down"
  | "delete.back"
  | "delete.forward"
  | "delete.wordBack"
  | "delete.toEnd"
  | "delete.toStart"
  | "delete.line"
  /**
   * Leave the open subagent and re-open the session that spawned it.
   *
   * Drill-in had no inverse. You reach a subagent by opening it from the rail or a
   * report card, and once there `esc` stops a turn and `^s` lists every session in
   * the install — neither is "back". `←` is the inverse of the `→` that got you in,
   * which is the grammar the panel already uses (`move.in`/`move.out`).
   */
  | "session.out"
  // -- the live work rail ---------------------------------------------------
  | "rail.enter"
  | "rail.up"
  | "rail.down"
  | "rail.open"
  | "rail.exit"
  /**
   * Stop the unit under the rail cursor — a background shell, a subagent, a run.
   *
   * The ONE keyboard route to stopping something that is not the open turn. Before
   * this there was none: `bg_7 ⋯ running sleep 90` sat for ninety seconds and the
   * only exits were waiting and `^c ^c`, which quits bough. Destructive, so the
   * dispatcher ARMS on the first press (printing the scope — spec §7) and acts on
   * the second; the keymap binds one chord because arming is a state of the rail,
   * not a second key.
   */
  | "rail.stop"
  // -- one job's output, opened from the rail -------------------------------
  /**
   * Leave the job view for the rail it was opened from.
   *
   * Back to the RAIL and not to chat: you opened this row to look at it, and a
   * surface that dumps you two levels out is one you stop using for a glance.
   */
  | "job.close"
  /** Kill the job being watched — the rail's two-step `x`, on the open job. */
  | "job.stop"
  // -- a question hold ------------------------------------------------------
  | "ask.pick"
  | "ask.send"
  | "ask.decline"
  // -- list navigation, shared by every list the panel holds ----------------
  | "move.up"
  | "move.down"
  | "move.in"
  | "move.out"
  /** A screenful of rows. The model tab is 32 rows in a 20-row viewport. */
  | "move.pageUp"
  | "move.pageDown"
  // -- MCP registration and authorization (spec §10) ------------------------
  /**
   * Start the OAuth flow for the selected server and print the URL to open.
   *
   * There was no key for this and no command either. An unauthorized server's
   * error said "not authorized — /mcp auth <name>", naming a slash command that
   * does not exist in this client — `/mcp` is a PANEL. So the one instruction the
   * product gave for the one action that unblocks a remote server was unfollowable,
   * and `beginMcpAuth`/`clearMcpAuth` sat in `api.ts` with no caller.
   */
  | "mcp.auth"
  /** Drop stored credentials for the selected server, so the next call re-authorizes. */
  | "mcp.forget"
  | "mcp.remove"
  | "mcp.connect"
  /** Register a remote server by URL — the buffer, not the write. */
  | "mcp.add"
  // -- workflow steering (spec §8) -----------------------------------------
  | "wf.pause"
  | "wf.resume"
  | "wf.stop"
  | "wf.rerun"
  /** The run's script — Workflows level 4, which nothing could reach. */
  | "wf.script"
  /** Save the open run's script under its name, so it can be run again. */
  | "wf.save"
  /** Cycle the agent-status filter (`WF_FILTERS`) on a big run. */
  | "wf.filter"
  /** Open the session of the agent under the cursor. The detail row promises it. */
  | "wf.openAgent"
  // -- the changes tab (spec §7: destructive, so it says its scope out loud) -
  /** Arm a revert of the file under the cursor. Nothing is written until ⏎. */
  | "changes.revert"
  /** Arm a revert of the WHOLE change set, in one key rather than two presses. */
  | "changes.revertAll";

// ---------------------------------------------------------------------------
// The tabs of the one panel
// ---------------------------------------------------------------------------

/**
 * Every non-chat surface, as data (spec §15).
 *
 * It lives HERE, in the keymap, and not in `Panel.tsx`, because a tab and its
 * direct-jump chord are the same fact: `TABS` is what `Command` derives `tab.*`
 * from, what `BINDINGS` binds, and what the help overlay prints. Adding a surface
 * is adding a row — it cannot add a mode, an open flag, or an escape path, and it
 * cannot ship without a key. `Panel.tsx` imports this and re-exports it; the
 * dependency points that way and never back, so this module stays free of ink.
 */
export const TABS = [
  // ONE tree, not two tabs. `sessions` was a flat recency list that knew nothing
  // about what was inside a conversation, and `tree` was bimodal — turns with one
  // open, lineage with none — so the same chord produced two unrelated screens.
  // `^s` stays bound to this tab as an alias below: it is the chord in everyone's
  // fingers for "where are my conversations", and that is still what this answers.
  { id: "tree", title: "tree", chord: "ctrl+f", desc: "conversations, turns, branches" },
  { id: "changes", title: "changes", chord: "ctrl+d", desc: "what this session changed" },
  { id: "workflows", title: "workflows", chord: "ctrl+w", desc: "workflow runs" },
  { id: "model", title: "model", chord: "ctrl+o", desc: "frontier · cheap · thinking depth" },
  { id: "mcp", title: "mcp", chord: "ctrl+p", desc: "servers, grants, authorization" },
  { id: "skills", title: "skills", chord: "ctrl+k", desc: "installed /skills" },
  { id: "theme", title: "theme", chord: "ctrl+y", desc: "browse live; leaving reverts" },
] as const satisfies readonly { id: string; title: string; chord: string; desc: string }[];

export type TabDef = (typeof TABS)[number];
export type PanelTab = TabDef["id"];
export type TabCommand = `tab.${PanelTab}`;

/** Tab ids in bar order. Derived, so the bar and the keymap cannot disagree. */
export const PANEL_TABS: readonly PanelTab[] = TABS.map((t) => t.id);

/** One `/name` row in the composer's popup: a command, not text to insert. */
export interface SlashCommand {
  /** What the user types after `/`. */
  name: string;
  command: Command;
  desc: string;
  /**
   * Whether trailing text is an ARGUMENT rather than prose.
   *
   * Off for almost everything, and that default is load-bearing: `/help me name this`
   * is a sentence about a command, not a command, and dispatching it would swallow a
   * message. It is on only where the trailing text has an obvious meaning the command
   * needs — `/compact focus on the parser`.
   */
  takesArg?: boolean;
}

/**
 * The built-in `/commands`.
 *
 * The `/` popup used to list SKILLS and nothing else, so typing `/model` — the
 * first thing anyone arriving from another harness does — answered "no matching
 * skills", and every surface bough has was reachable only by a memorised chord.
 * Typing `/` is how peer harnesses expose their surface area; the chords stay, and
 * this is the other door to the same commands.
 *
 * Derived from `TABS` for the same reason the chords are: a tab and the ways to
 * reach it are one fact, so a new surface cannot ship without a name to type.
 * Only the two commands that are not a tab are listed by hand.
 */
export const SLASH_COMMANDS: readonly SlashCommand[] = [
  ...TABS.map((t) => ({ name: t.id, command: `tab.${t.id}` as Command, desc: t.desc })),
  { name: "new", command: "session.new", desc: "start a fresh conversation" },
  {
    name: "compact",
    command: "session.compact",
    desc: "hand off to a fresh conversation · /compact <goal>",
    takesArg: true,
  },
  { name: "rewind", command: "tree.rewind", desc: "go back to a turn and say it differently" },
  { name: "schedules", command: "schedules.show", desc: "the recurring runs and when they fire" },
  { name: "saved", command: "saved.show", desc: "workflows saved to run again by name" },
  { name: "artifacts", command: "artifacts.show", desc: "pages this conversation published" },
  { name: "help", command: "help.open", desc: "every key, by section" },
];

/**
 * The command a DRAFT names, if the whole draft is one — `"/model"` → `tab.model`.
 *
 * WHY THE SEND PATH NEEDS THIS AT ALL. `/` commands used to fire from exactly one
 * place: accepting a row in the completion popup. The popup opens as you type, so
 * text that arrives faster than a render — a paste, a fast typist, anything that
 * delivers the line and its Return in one read — never opened it, and Enter then
 * sent `/model` to the frontier model as an ordinary sentence. Measured: 19k
 * tokens, a real charge, and a conversation auto-titled "Model Architecture
 * Discussion". Typing it slowly worked, which made it intermittent and therefore
 * harder to believe.
 *
 * EXACT AND WHOLE, deliberately. `/model` dispatches; `/help me name this` is
 * prose about a command and is sent, because a message that begins with a command
 * name is not the same thing as a command. Skills are not here either — a skill
 * reference is text the model reads (`/prewalk fix the parser`), so it must stay
 * in the message.
 */
export function slashCommandFor(draft: string): Command | null {
  const m = /^\/([a-z][a-z0-9-]*)$/i.exec(draft.trim());
  if (!m) return null;
  const name = m[1].toLowerCase();
  return SLASH_COMMANDS.find((c) => c.name === name)?.command ?? null;
}

/**
 * The commands other harnesses have that bough answers under a different name.
 *
 * NOT aliases — deliberately. Silently mapping `/clear` onto `/new` would run a
 * destructive-looking command the user did not choose, on a guess about which product
 * they came from. A suggestion tells them the name and lets them press it.
 */
const FOREIGN_COMMANDS: Record<string, string> = {
  clear: "new",
  reset: "new",
  resume: "tree",
  sessions: "tree",
  history: "tree",
  cost: "model",
  status: "model",
  diff: "changes",
  exit: "",
  quit: "",
};

/**
 * A bare `/word` that is NOT a command, with the nearest thing that is.
 *
 * WHY THIS EXISTS AT ALL. A draft of exactly `/something` is a command attempt, and
 * bough used to send an unrecognised one to the frontier model as prose. Measured on a
 * Claude Code habit: `/clear` reached haiku, which answered "Done. State cleared." and
 * offered to revert the workspace's modified files — a made-up confirmation for an
 * operation that never happened, and a near miss with the user's uncommitted work.
 * Nothing about that failure is visible to the user; it looks like it worked.
 *
 * Returns `null` for anything that IS a command, names a skill, or is not a lone
 * `/word` — `/prewalk fix the parser` is text the model reads, and prose that merely
 * begins with a slash is prose.
 */
export function unknownCommand(
  draft: string,
  skills: readonly string[] = [],
): { name: string; suggestion: string | null } | null {
  const m = /^\/([a-z0-9][a-z0-9:_-]*)$/i.exec(draft.trim());
  if (!m) return null;
  const name = m[1].toLowerCase();
  if (SLASH_COMMANDS.some((c) => c.name === name)) return null;
  if (skills.some((s) => s.toLowerCase() === name)) return null;
  const foreign = FOREIGN_COMMANDS[name];
  if (foreign !== undefined) return { name, suggestion: foreign || null };
  // Nearest command by prefix, then by containment. Skills are candidates too: a
  // mistyped skill name is the other half of this mistake.
  const candidates = [...SLASH_COMMANDS.map((c) => c.name), ...skills];
  const near = candidates.find((c) => c.startsWith(name)) ??
    candidates.find((c) => c.includes(name));
  return { name, suggestion: near ?? null };
}

/**
 * A draft as an INVOCATION: the command it names plus its argument./**
 * A draft as an INVOCATION: the command it names plus its argument.
 *
 * `slashCommandFor` answers the exact case and is what the strict no-argument
 * commands need. This is the same match widened to the commands that declare
 * `takesArg`, so `/compact focus on the parser` reaches the handoff with a goal
 * instead of being sent to the model as a sentence beginning with a command name.
 */
export function slashInvocation(draft: string): { command: Command; arg: string } | null {
  const trimmed = draft.trim();
  const exact = slashCommandFor(trimmed);
  if (exact) return { command: exact, arg: "" };
  const m = /^\/([a-z][a-z0-9-]*)\s+([\s\S]+)$/i.exec(trimmed);
  if (!m) return null;
  const spec = SLASH_COMMANDS.find((c) => c.name === m[1].toLowerCase());
  if (!spec?.takesArg) return null;
  return { command: spec.command, arg: m[2].trim() };
}

/** Opens and closes the panel. Never names a tab — that is what the others are for. */
export const PANEL_TOGGLE = "ctrl+t";

/** The tab a chord jumps to, or `null`. */
export function tabForChord(chord: string): PanelTab | null {
  return TABS.find((t) => t.chord === chord)?.id ?? null;
}

/** The tab a `tab.*` command names, or `null` for every other command. */
export function tabForCommand(command: Command): PanelTab | null {
  if (!command.startsWith("tab.")) return null;
  const id = command.slice(4) as PanelTab;
  return PANEL_TABS.includes(id) ? id : null;
}

// ---------------------------------------------------------------------------
// Chords (pure)
// ---------------------------------------------------------------------------

/** The subset of ink's `Key` this module reads. Structural, so ink's own type fits. */
export interface KeyFlags {
  upArrow?: boolean;
  downArrow?: boolean;
  leftArrow?: boolean;
  rightArrow?: boolean;
  pageUp?: boolean;
  pageDown?: boolean;
  home?: boolean;
  end?: boolean;
  return?: boolean;
  escape?: boolean;
  tab?: boolean;
  backspace?: boolean;
  delete?: boolean;
  ctrl?: boolean;
  shift?: boolean;
  meta?: boolean;
  super?: boolean;
}

/**
 * One keypress as a canonical string — `"ctrl+p"`, `"meta+enter"`, `"esc"`, `"?"`.
 *
 * Returns `""` for anything that is not a chord: a paste, a coalesced chunk of
 * typing, a bare modifier. The caller treats that as text, which is what keeps a
 * multi-character stdin read from being matched against the table by accident.
 */
export function chordOf(input: string, key: KeyFlags = {}): string {
  const mods: string[] = [];
  if (key.ctrl) mods.push("ctrl");
  if (key.meta) mods.push("meta");
  if (key.super) mods.push("super");

  let base: string;
  if (key.upArrow) base = "up";
  else if (key.downArrow) base = "down";
  else if (key.leftArrow) base = "left";
  else if (key.rightArrow) base = "right";
  else if (key.pageUp) base = "pageup";
  else if (key.pageDown) base = "pagedown";
  else if (key.home) base = "home";
  else if (key.end) base = "end";
  else if (key.escape) base = "esc";
  else if (key.tab) base = "tab";
  else if (key.backspace || key.delete) base = "backspace";
  else if (key.return) base = "enter";
  // A raw "\n" with no return flag can only be ^j. Terminals send \r for Return,
  // so this is the newline chord even on terminals that report no ctrl modifier
  // for it — the old tree shipped a bug where ^j submitted half a message.
  else if (input === "\n") return "ctrl+j";
  else if (input === " ") base = "space";
  else if (input.length === 1) base = input;
  else return "";

  if (key.shift && (base === "enter" || base === "tab")) mods.push("shift");
  return mods.length > 0 ? `${mods.join("+")}+${base}` : base;
}

const CHORD_GLYPH: Record<string, string> = {
  ctrl: "^",
  meta: "⌥",
  super: "⌘",
  shift: "⇧",
  up: "↑",
  down: "↓",
  left: "←",
  right: "→",
  enter: "⏎",
  esc: "esc",
  tab: "⇥",
  backspace: "⌫",
  pageup: "pgup",
  pagedown: "pgdn",
  space: "space",
};

/** A chord as the help overlay prints it: `"ctrl+p"` → `"^p"`. */
export function chordLabel(chord: string): string {
  const parts = chord.split("+");
  const base = parts.pop() ?? "";
  const mods = parts.map((m) => CHORD_GLYPH[m] ?? m).join("");
  return mods + (CHORD_GLYPH[base] ?? base);
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/**
 * What a binding can be conditioned on. Every field is a plain boolean the caller
 * already knows, so a guard costs nothing to evaluate and nothing to fake.
 */
export interface KeyContext {
  mode: UiMode;
  /**
   * The panel's open tab, or null when the panel is closed.
   *
   * NOT a boolean, and therefore not a `Guard`: it is the STRUCTURAL scope a
   * tab-local binding is matched against. A bare letter in the panel means whatever
   * the open tab says it means — `x` stops a run in `workflows` and arms a revert in
   * `changes` — and until this field existed the table could only say so in prose.
   * It did, and the dispatcher did not read prose: `p`, `P` and `r` carried
   * "(workflows)" in their descriptions and steered `state.workflows[sel]` from every
   * tab, acting on a row the user was not looking at. Scope is data now, resolved by
   * `lookup`, so a binding that names a tab cannot fire outside it.
   *
   * OPTIONAL, and absent means the same as `null`: the panel is closed. A caller
   * that omits it reaches no tab-local binding at all — `x`, `X`, `s`, `p`, `P`,
   * `r`, `e`, `f` and `o` resolve to nothing. That is the SAFE degrade (a bare
   * letter that does nothing beats one that stops a run you are not looking at),
   * but it is a degrade: the dispatcher must pass the open tab.
   */
  tab?: PanelTab | null;
  /** The composer is empty, so a chord can mean something other than editing. */
  emptyDraft: boolean;
  /**
   * The open session was spawned by another one, so there is somewhere to go back
   * to. Gates `←` on an empty draft; with text in the draft `←` is still the cursor.
   *
   * OPTIONAL and absent means false — a caller that omits it simply never reaches
   * `session.out`, which is the safe degrade (the arrow keeps editing).
   */
  inSubagent?: boolean;
  /** The draft spans more than one line: ↑/↓ move the cursor, not history. */
  multiline: boolean;
  /** A turn is in flight in the open session. */
  busy: boolean;
  /** The previous Escape landed inside the double-tap window. */
  doubleEsc: boolean;
  /** A ^c is already pending — the next one quits (spec: ^c ^c quits). */
  quitArmed: boolean;
  /** At least one subagent is working, so ↓ can drop into the rail. */
  railLive: boolean;
  /**
   * The `@`/`/` popup is open with at least one row.
   *
   * A GUARD rather than a mode, because the composer keeps the keyboard the whole
   * time: ↑/↓ walk the popup while it is open and history when it is not, and tab
   * accepts a row or does nothing. Making it a mode would mean an escape path,
   * and there already is one — the popup closes on esc like any other transient.
   */
  completing: boolean;
  /**
   * The panel's filter buffer has the keyboard (`/` opened it).
   *
   * The reason type-to-filter could not simply be switched on: `p`, `P`, `x`, `r`,
   * `s`, `j` and `k` are all live letters in the panel, so a user typing a model
   * name would have paused a workflow and pinned a model on the way through. The
   * resolution is MODAL and it is the one Sessions already drew — `/` opens a
   * buffer, and while the buffer is open every bare letter and digit in the panel is
   * text. One rule, every tab, and no letter has two meanings that depend on how
   * fast you typed.
   *
   * Optional for the same reason `tab` is, and with the same safe degrade: omitted
   * reads as "not filtering", which is what every surface outside the panel is.
   */
  panelFiltering?: boolean;
}

/**
 * The boolean fields of `KeyContext` — everything a `when`/`not` can name.
 *
 * `tab` is excluded because it is not a flag: it is matched by `Binding.tab`, which
 * is a set membership rather than a truth test.
 */
export type Guard = Exclude<keyof KeyContext, "mode" | "tab">;

export interface Binding {
  /** `"*"` binds in every mode — the handful of chords that must always work. */
  mode: UiMode | "*";
  /** Canonical chord, as `chordOf` produces it. */
  chord: string;
  command: Command;
  /** Every named flag must be true. */
  when?: Guard[];
  /** Every named flag must be false. */
  not?: Guard[];
  /**
   * Panel tabs this row is live in. Absent = every tab (and the closed panel).
   *
   * The structural half of "a bare letter means what the open tab says it means".
   * Two rows may share a chord as long as their tab sets are disjoint, which is how
   * `x` is `wf.stop` in one tab and `changes.revert` in another without either being
   * dead.
   */
  tab?: readonly PanelTab[];
  /** Help section. A binding with no section is an alias and is not documented. */
  section?: string;
  /** Terse: the overlay lays sections out in two columns, ~35 columns each. */
  desc?: string;
  /** Overrides the printed chord, for a run of rows that share one description. */
  label?: string;
}

/** The help section the direct-jump chords are printed under. */
const PANEL_SECTION = "the panel — ^t, or jump straight to a tab";

/**
 * Tabs whose body is a flat list long enough to need narrowing.
 *
 * `changes`, `tree`, `workflows` and `mcp` are structured views with their own
 * drill-in, and `theme` is eight rows; a filter there would be a second way to move
 * a cursor rather than a way to find a row. `model` is thirty-two rows in a
 * twenty-row viewport and `skills` grows with the install, which is the case this
 * exists for.
 */
export const FILTER_TABS: readonly PanelTab[] = ["tree", "model", "skills"];

/**
 * Chords that reach the panel from outside it and move between its tabs inside it.
 *
 * The four chords that a composer already owns (`^f` forward, `^d` delete, `^w`
 * word-back, `^k` kill) are guarded on an empty draft, so typing keeps working and
 * a jump is still one key when there is nothing to type. The other five collide
 * with nothing and are therefore NOT guarded: a panel you cannot open because you
 * have a half-written message is a panel with a hidden precondition.
 *
 * Generated from `TABS` rather than written out, which is what makes "every tab has
 * a chord" true by construction instead of by review.
 */
function panelChords(): Binding[] {
  const composerOwned = new Set(["ctrl+f", "ctrl+d", "ctrl+w", "ctrl+k"]);
  const rows: Binding[] = [];
  for (
    const [chord, command, desc] of [
      [PANEL_TOGGLE, "panel.toggle", "open / close the panel"] as const,
      ...TABS.map((t) => [t.chord, `tab.${t.id}` as TabCommand, t.desc] as const),
    ]
  ) {
    // Documented once, on the chat row — the overlay is read from chat.
    rows.push({
      mode: "chat",
      chord,
      command,
      ...(composerOwned.has(chord) ? { when: ["emptyDraft" as Guard] } : {}),
      section: PANEL_SECTION,
      desc,
    });
    // A direct jump must work from anywhere it is not being typed into.
    rows.push({ mode: "panel", chord, command });
    rows.push({ mode: "rail", chord, command });
    // INCLUDING while a question is held. `ask` was left out, so a held `ask()` swallowed
    // every panel chord and the answer had to be given blind — worst of all on the
    // workflow approval card, whose own text says "`x` in the workflows tab (^w) stops a
    // run at any point" about a tab that could not be opened until the card was answered.
    // The card sits in the composer's place, so the panel displaces the transcript and the
    // question stays on screen beside it.
    rows.push({ mode: "ask", chord, command });
  }
  // `^s` is the chord everyone's fingers already have for "where are my
  // conversations", and that question is now answered by the tree. Kept as an alias
  // rather than retired: the tab it used to open no longer exists, and a chord that
  // silently does nothing is worse than one that lands somewhere sensible.
  // DOCUMENTED, not just bound. It carried no `desc`, so `helpSections` skipped it
  // and the overlay never mentioned `^s` at all — nor did the `not bound` list,
  // which is where a reader who tried it would look next. A reflex chord that is
  // live, undocumented, and lands you in a surface that owns the keyboard is the
  // worst of the three states it could be in.
  for (const mode of ["chat", "panel", "rail", "ask"] as const) {
    rows.push({
      mode,
      chord: SESSIONS_ALIAS,
      command: "tab.tree",
      ...(mode === "chat"
        ? { when: ["emptyDraft" as Guard], section: PANEL_SECTION, desc: "the tree, too" }
        : {}),
    });
  }
  return rows;
}

/** The retired `sessions` tab's chord, still bound to the tree that replaced it. */
const SESSIONS_ALIAS = "ctrl+s";

/**
 * `1`…`9`, one row each, documented once as `1-9`.
 *
 * `extra` carries the guards a digit row needs where the surface has other uses for
 * a bare keypress — in the panel, a digit is only a pick while the filter buffer is
 * closed.
 */
const digits = (
  mode: UiMode | "*",
  command: Command,
  section: string,
  desc: string,
  extra: Partial<Binding> = {},
): Binding[] =>
  Array.from({ length: 9 }, (_v, i) => ({
    mode,
    chord: String(i + 1),
    command,
    ...extra,
    ...(i === 0 ? { section, desc, label: "1-9" } : {}),
  }));

/**
 * Every binding in the TUI, in resolution order within a mode.
 *
 * Ordering is only ever used to put a GUARDED row ahead of its unguarded fallback
 * (`^f` opens the tree on an empty composer, moves the cursor otherwise). Two rows
 * that could both match the same context is a bug the duplicate test catches.
 */
export const BINDINGS: Binding[] = [
  // -- global ---------------------------------------------------------------
  // Two rows, because a SINGLE ^c must not quit: the first arms the hint, the
  // second acts on it. Bound in every mode — a panel you cannot leave is worse
  // than one you never opened, and ^c is the key everyone reaches for.
  {
    mode: "*",
    chord: "ctrl+c",
    command: "quit",
    when: ["quitArmed"],
    section: "leaving",
    label: "^c ^c",
    desc: "quit · subagents keep running",
  },
  { mode: "*", chord: "ctrl+c", command: "quit.arm" },

  // -- chat -----------------------------------------------------------------
  {
    mode: "chat",
    chord: "?",
    command: "help.open",
    when: ["emptyDraft"],
    section: "leaving",
    desc: "this overlay",
  },

  // The popup owns ⏎ while it is open, so it sits AHEAD of `send`. "Enter commits"
  // is the one rule that makes every bordered-list-with-a-cursor in the TUI
  // learnable once, and the pickers were the only widget that broke it: ⏎ on a
  // highlighted `/history` row discarded the row and sent the literal draft `/` as
  // a turn. ⇥ stays bound as the alias a completion menu is also expected to have.
  {
    mode: "chat",
    chord: "enter",
    command: "complete.accept",
    when: ["completing"],
    section: "compose",
    label: "⏎ ⇥",
    desc: "accept the @ or / suggestion",
  },
  {
    mode: "chat",
    chord: "enter",
    command: "send",
    section: "compose",
    desc: "send · interjects while a turn runs",
  },
  {
    mode: "chat",
    chord: "meta+enter",
    command: "send.queue",
    section: "compose",
    desc: "queue for after this turn",
  },
  { mode: "chat", chord: "ctrl+j", command: "newline", section: "compose", desc: "newline" },
  {
    mode: "chat",
    chord: "ctrl+n",
    command: "session.new",
    section: "compose",
    desc: "start a fresh conversation",
  },
  // `not: ["emptyDraft"]` is not decoration: with nothing typed there is nothing to
  // clear, and a double-tap that resolved here anyway SWALLOWED the gesture — a
  // user hammering Escape at a running turn got "cleared an empty draft" instead of
  // "stopped it". Falling through lets the rows below answer, which is the honest
  // reading of "esc esc clears the draft".
  {
    mode: "chat",
    chord: "esc",
    command: "draft.clear",
    when: ["doubleEsc"],
    not: ["emptyDraft"],
    section: "compose",
    label: "esc esc",
    desc: "clear the draft",
  },
  /**
   * esc esc with NOTHING typed and NOTHING running: open the tree on your last turn.
   *
   * The gesture already meant "undo the thing I am in the middle of" — it cleared a
   * draft, it stopped a turn. With an empty composer and an idle session it meant
   * "dismiss a notice", which is nothing at all, and the actual undo one reaches for
   * at that moment — go back a message and say it differently — was four keypresses
   * into a panel tab. It is one gesture now, and it lands on the turn (`rewindIndex`)
   * rather than at the top of the forest.
   *
   * Ordered AHEAD of `turn.interrupt` is deliberate but safe: `not: ["busy"]` means a
   * running turn still resolves to the stop below, which is the meaning nobody may
   * lose.
   */
  {
    mode: "chat",
    chord: "esc",
    command: "tree.rewind",
    when: ["doubleEsc", "emptyDraft"],
    not: ["busy", "completing"],
    section: "compose",
    label: "esc esc",
    desc: "go back to a turn and fork it",
  },
  // The @// popup, while it is open. These sit AHEAD of the composer's own ↑/↓ and
  // esc — and, since the popup's own legend row promises `esc closes`, ahead of
  // `turn.interrupt` too. The earlier order argued that stopping a turn outranks
  // closing a menu, but the legend was never told: one Escape with the skills picker
  // open during a turn killed the turn and left the picker on screen still saying
  // `esc closes`. Escape unwinds exactly ONE level, nearest surface first, which is
  // what every other surface in this TUI already does.
  { mode: "chat", chord: "tab", command: "complete.accept", when: ["completing"] },
  // ⇥ with no popup takes the ghost. Ordered AFTER the popup row, so a menu always wins the
  // key it advertises; documented on the compose row that already names ⇥.
  {
    mode: "chat",
    chord: "tab",
    command: "ghost.accept",
    not: ["completing"],
    section: "compose",
    desc: "take the suggested next message",
  },
  { mode: "chat", chord: "up", command: "complete.prev", when: ["completing"] },
  { mode: "chat", chord: "down", command: "complete.next", when: ["completing"] },
  { mode: "chat", chord: "esc", command: "complete.dismiss", when: ["completing"] },
  // Spec §5's user interrupt. Ordered between the popup above and the plain `cancel`
  // below, which is the whole reason the table resolves top-down: while a turn is
  // running, one Escape stops it; with nothing running it dismisses a notice.
  // Guarded on `busy` rather than bound to a chord of its own because Escape is the
  // key every user already reaches for to stop something, and a stop button nobody
  // finds is the gap this closes, not a smaller version of it.
  {
    mode: "chat",
    chord: "esc",
    command: "turn.interrupt",
    when: ["busy"],
    section: "leaving",
    desc: "stop the running turn",
  },
  { mode: "chat", chord: "esc", command: "cancel" },
  {
    mode: "chat",
    chord: "up",
    command: "cursor.up",
    when: ["multiline"],
    section: "compose",
    label: "↑/↓",
    desc: "history · lines if multiline",
  },
  { mode: "chat", chord: "up", command: "history.prev" },
  { mode: "chat", chord: "down", command: "cursor.down", when: ["multiline"] },
  {
    mode: "chat",
    chord: "down",
    command: "rail.enter",
    when: ["emptyDraft", "railLive"],
    section: "read",
    desc: "into the live work rail",
  },
  { mode: "chat", chord: "down", command: "history.next" },

  // -- reading --------------------------------------------------------------
  {
    mode: "chat",
    chord: "ctrl+e",
    command: "fold.all",
    when: ["emptyDraft"],
    section: "read",
    desc: "fold/unfold every tool call",
  },
  {
    mode: "chat",
    chord: "pageup",
    command: "scroll.pageUp",
    section: "read",
    label: "pgup pgdn",
    desc: "scroll back / forward",
  },
  { mode: "chat", chord: "pagedown", command: "scroll.pageDown" },

  // -- the one tabbed panel -------------------------------------------------
  ...panelChords(),

  // -- editing the line -----------------------------------------------------
  {
    mode: "chat",
    chord: "ctrl+a",
    command: "cursor.home",
    section: "edit the line",
    label: "^a ^e",
    desc: "line start / end",
  },
  { mode: "chat", chord: "ctrl+e", command: "cursor.end" },
  { mode: "chat", chord: "home", command: "cursor.home" },
  { mode: "chat", chord: "end", command: "cursor.end" },
  // AHEAD of `cursor.left`, and guarded on an empty draft: with text in the line
  // `←` is still the cursor, so leaving a subagent can never eat a keystroke you
  // meant for the draft.
  {
    mode: "chat",
    chord: "left",
    command: "session.out",
    when: ["emptyDraft", "inSubagent"],
    section: "read",
    label: "←",
    desc: "back to the session that spawned this one",
  },
  { mode: "chat", chord: "left", command: "cursor.left" },
  { mode: "chat", chord: "right", command: "cursor.right" },
  {
    mode: "chat",
    chord: "ctrl+b",
    command: "cursor.left",
    section: "edit the line",
    label: "^b ^f",
    desc: "char back / forward",
  },
  { mode: "chat", chord: "ctrl+f", command: "cursor.right" },
  {
    mode: "chat",
    chord: "meta+b",
    command: "cursor.wordLeft",
    section: "edit the line",
    label: "⌥b ⌥f",
    desc: "word back / forward",
  },
  { mode: "chat", chord: "meta+f", command: "cursor.wordRight" },
  { mode: "chat", chord: "meta+left", command: "cursor.wordLeft" },
  { mode: "chat", chord: "meta+right", command: "cursor.wordRight" },
  {
    mode: "chat",
    chord: "ctrl+d",
    command: "delete.forward",
    section: "edit the line",
    label: "^d · ^w",
    desc: "delete char ahead · word behind",
  },
  { mode: "chat", chord: "ctrl+w", command: "delete.wordBack", not: ["emptyDraft"] },
  { mode: "chat", chord: "meta+backspace", command: "delete.wordBack" },
  {
    mode: "chat",
    chord: "ctrl+k",
    command: "delete.toEnd",
    section: "edit the line",
    label: "^k ^u",
    desc: "kill to end / whole line",
  },
  { mode: "chat", chord: "ctrl+u", command: "delete.line" },
  {
    mode: "chat",
    chord: "super+backspace",
    command: "delete.toStart",
    section: "edit the line",
    label: "⌘⌫ ⌘←→",
    desc: "to line start · jump to ends",
  },
  { mode: "chat", chord: "super+left", command: "cursor.home" },
  { mode: "chat", chord: "super+right", command: "cursor.end" },
  { mode: "chat", chord: "backspace", command: "delete.back" },

  // -- the live subagent rail ----------------------------------------------
  {
    mode: "rail",
    chord: "up",
    command: "rail.up",
    section: "the rail",
    label: "↑/↓",
    desc: "move",
  },
  { mode: "rail", chord: "down", command: "rail.down" },
  {
    mode: "rail",
    chord: "enter",
    command: "rail.open",
    section: "the rail",
    desc: "open this agent / shell output",
  },
  {
    mode: "rail",
    chord: "esc",
    command: "rail.exit",
    section: "the rail",
    desc: "back to the composer",
  },
  // The rail is where running work is listed, so it is where running work is
  // stopped. `x` is the same letter the workflows tab already uses for "stop", and
  // the same two-step: the first press names what will be killed, the second does
  // it (spec §7 — consent is never inferred).
  {
    mode: "rail",
    chord: "x",
    command: "rail.stop",
    section: "the rail",
    label: "x x",
    desc: "stop this shell / agent / run",
  },

  // -- a question hold ------------------------------------------------------
  ...digits("ask", "ask.pick", "when bough asks", "pick an option"),
  {
    mode: "ask",
    chord: "enter",
    command: "ask.send",
    section: "when bough asks",
    desc: "send what you typed",
  },
  {
    mode: "ask",
    chord: "esc",
    command: "ask.decline",
    section: "when bough asks",
    desc: "decline (the program catches it)",
  },

  // -- inside the panel -----------------------------------------------------
  // One set of navigation keys for eight tabs. What ⏎ affirms depends on the tab
  // (`PanelHost.tsx` dispatches it), which is why there is one `panel.confirm` and
  // not one command per tab: the tab already decides, and a second place that
  // decides is a second place to disagree.
  {
    mode: "panel",
    chord: "up",
    command: "move.up",
    section: "inside the panel",
    label: "↑↓ j/k",
    desc: "move",
  },
  { mode: "panel", chord: "down", command: "move.down" },
  // Bare letters, so they are text while the filter buffer has the keyboard.
  { mode: "panel", chord: "k", command: "move.up", not: ["panelFiltering"] },
  { mode: "panel", chord: "j", command: "move.down", not: ["panelFiltering"] },
  // A screenful. The model tab is thirty-two rows in a twenty-row viewport and ↑↓
  // walk it one row at a time; the transcript and the help overlay both already
  // page, and a list that does not is the only list in the TUI that does not.
  {
    mode: "panel",
    chord: "pageup",
    command: "move.pageUp",
    section: "inside the panel",
    label: "pgup pgdn",
    desc: "a screenful at a time",
  },
  { mode: "panel", chord: "pagedown", command: "move.pageDown" },
  {
    mode: "panel",
    chord: "tab",
    command: "panel.next",
    section: "inside the panel",
    label: "⇥ ⇧⇥",
    desc: "next / previous tab",
  },
  { mode: "panel", chord: "shift+tab", command: "panel.prev" },
  {
    mode: "panel",
    chord: "enter",
    command: "panel.confirm",
    section: "inside the panel",
    desc: "open · grant · keep — what the tab affirms",
  },
  {
    mode: "panel",
    chord: "right",
    command: "move.in",
    section: "inside the panel",
    label: "→ ←",
    desc: "drill into delegated work (tree)",
  },
  { mode: "panel", chord: "left", command: "move.out" },
  // Spec §3: a list is addressable BY DIGIT, not only arrowable. The dispatcher
  // reads the digit off the keypress, exactly as the ask card does.
  ...digits("panel", "panel.pick", "inside the panel", "jump to that row and affirm it", {
    not: ["panelFiltering"],
  }),
  // A letter, like the workflow steering keys below and for the same reason: the
  // panel has the keyboard while it is open. `tab` — not the description — is what
  // keeps it inside the tree tab.
  {
    mode: "panel",
    chord: "s",
    command: "panel.confirmSummarize",
    tab: ["tree"],
    not: ["panelFiltering"],
    section: "inside the panel",
    desc: "tree: branch, carrying a summary of what you left",
  },

  // -- type-to-filter, the panel's one modal buffer -------------------------
  // MODAL, and deliberately: bare-letter filtering cannot coexist with `p`/`x`/`r`/
  // `s`/`j`/`k`, and Sessions already draws a `/ filter` row. So one gesture, in
  // every list tab that has one, and while it is open a letter is a letter.
  {
    mode: "panel",
    chord: "/",
    command: "panel.filter",
    tab: FILTER_TABS,
    not: ["panelFiltering"],
    section: "inside the panel",
    // Naming the tree case is not padding: in every other tab `/` narrows the rows
    // on screen, but in the tree it is a FULL-TEXT search of every message that
    // expands the turns it hit — the single most useful thing in the panel, and
    // "filter this list" is not a sentence anyone reads as "search my history".
    desc: "filter this list — in the tree, searches every message · esc clears",
  },
  { mode: "panel", chord: "backspace", command: "panel.filterBack", when: ["panelFiltering"] },
  // Ahead of `panel.close`: escape unwinds exactly ONE level, nearest surface
  // first, which is the rule every other surface in this TUI already follows.
  { mode: "panel", chord: "esc", command: "panel.filterExit", when: ["panelFiltering"] },
  {
    mode: "panel",
    chord: "esc",
    command: "panel.close",
    section: "inside the panel",
    desc: "back to chat",
  },

  // -- workflow runs (spec §8: pause, stop, relaunch from the journal) ------
  // The MCP tab's verbs. Registering a remote server used to mean hand-editing
  // `~/.bough/mcp.json` and restarting, and authorizing one had no route through
  // this client at all — the tab's whole legend was "⏎ grant/revoke".
  {
    mode: "panel",
    chord: "a",
    command: "mcp.auth",
    tab: ["mcp"],
    not: ["panelFiltering"],
    section: "the mcp tab",
    desc: "authorize — prints the URL to open",
  },
  {
    mode: "panel",
    chord: "n",
    command: "mcp.add",
    tab: ["mcp"],
    not: ["panelFiltering"],
    section: "the mcp tab",
    desc: "add a remote server by URL",
  },
  {
    mode: "panel",
    chord: "F",
    command: "mcp.forget",
    tab: ["mcp"],
    not: ["panelFiltering"],
    section: "the mcp tab",
    desc: "forget this server's credentials",
  },
  // PROOF, which the panel could not offer at all. Every other row here states an
  // intention — granted, keychain, needs auth — and none of them survives contact
  // with the server: "keychain" says which credential will be TRIED, and the only
  // way to find out whether it is accepted was to spend a turn on a tool call and
  // read the failure. `POST /mcp/servers/:name/connect` has always answered exactly
  // this and nothing in the TUI called it.
  {
    mode: "panel",
    chord: "c",
    command: "mcp.connect",
    tab: ["mcp"],
    not: ["panelFiltering"],
    section: "the mcp tab",
    desc: "test the connection · names the tools, or the error",
  },
  // RECOVERY. `c` says a stdio server is dead; nothing said how to bring it back. A
  // crashed or wedged subprocess kept answering every `mcp()` call with the same
  // failure until the whole bough server was restarted, because the manager caches the
  // child per session. `POST /mcp/servers/:name/restart` drops it and starts a new one,
  // and had no key.
  {
    mode: "panel",
    chord: "r",
    command: "mcp.restart",
    tab: ["mcp"],
    not: ["panelFiltering"],
    section: "the mcp tab",
    desc: "restart this server's process",
  },
  // Removing the ENTRY, which `F` deliberately does not do. The two were one verb in
  // everyone's head and neither was the other: `F` leaves the server registered and
  // unauthorized, so a server added by mistake — or a duplicate pointing at an
  // endpoint another entry already covers — could only be removed by hand-editing
  // `~/.bough/mcp.json`. Same two-step as every other destructive key (spec §7).
  {
    mode: "panel",
    chord: "d",
    command: "mcp.remove",
    tab: ["mcp"],
    not: ["panelFiltering"],
    section: "the mcp tab",
    desc: "delete this registration · d again confirms",
  },

  // Bound in the panel and live only in the workflows tab. They are letters rather
  // than chords because the panel has the keyboard when it is open, and they carry
  // `tab` because a letter that acts on a list you are not looking at is a bug the
  // dispatcher already had to patch by hand.
  {
    mode: "panel",
    chord: "p",
    command: "wf.pause",
    tab: ["workflows"],
    not: ["panelFiltering"],
    section: "the workflows tab",
    desc: "pause · in-flight agents finish",
  },
  {
    mode: "panel",
    chord: "P",
    command: "wf.resume",
    tab: ["workflows"],
    not: ["panelFiltering"],
    section: "the workflows tab",
    desc: "resume",
  },
  {
    mode: "panel",
    chord: "x",
    command: "wf.stop",
    tab: ["workflows"],
    not: ["panelFiltering"],
    section: "the workflows tab",
    desc: "stop · pause first to keep work",
  },
  {
    mode: "panel",
    chord: "r",
    command: "wf.rerun",
    tab: ["workflows"],
    not: ["panelFiltering"],
    section: "the workflows tab",
    desc: "relaunch from the journal",
  },
  // The three verbs the run view has always PRINTED and never bound: `steerActions`
  // offers "e script", the agent pane offers the `f` status cycle, and an agent's
  // detail row says "session <id> — o opens it". A promise on screen for a key that
  // does nothing is worse than no promise.
  {
    mode: "panel",
    chord: "e",
    command: "wf.script",
    tab: ["workflows"],
    not: ["panelFiltering"],
    section: "the workflows tab",
    desc: "the run's script",
  },
  // `saved.ts`, the REST routes and `api.saveWorkflowAs` have all existed since the
  // workflows tab landed; nothing on screen ever reached them, so a run you would
  // want again could only be re-obtained by asking the model to write it a second
  // time. One key closes it.
  {
    mode: "panel",
    chord: "s",
    command: "wf.save",
    tab: ["workflows"],
    not: ["panelFiltering"],
    section: "the workflows tab",
    desc: "save this run as a reusable workflow",
  },
  {
    mode: "panel",
    chord: "f",
    command: "wf.filter",
    tab: ["workflows"],
    not: ["panelFiltering"],
    section: "the workflows tab",
    desc: "cycle agents: all/running/queued/done/error",
  },
  {
    mode: "panel",
    chord: "o",
    command: "wf.openAgent",
    tab: ["workflows"],
    not: ["panelFiltering"],
    section: "the workflows tab",
    desc: "open this agent's session",
  },

  // -- the tree tab ---------------------------------------------------------
  // The server has had `POST /sessions/:id/extract` since the port with no key on it.
  // `⏎` on a turn FORKS (keep the prefix, redo from here); `e` is the other half —
  // keep the SUFFIX, as its own conversation — and neither destroys anything.
  {
    mode: "panel",
    chord: "e",
    command: "tree.extract",
    tab: ["tree"],
    not: ["panelFiltering"],
    section: "the tree tab",
    desc: "split here — this turn on becomes its own conversation",
  },
  {
    mode: "panel",
    chord: "m",
    command: "tree.moveInto",
    tab: ["tree"],
    not: ["panelFiltering"],
    section: "the tree tab",
    desc: "bring this turn on into the open conversation",
  },

  // -- the changes tab (spec §7) --------------------------------------------
  // `x` reached this tab only because it was bound to `wf.stop` and the dispatcher
  // re-routed it by hand, and `X` could not reach it at all. Both are their own
  // commands now, scoped by `tab` rather than by an `if` in the panel host.
  {
    mode: "panel",
    chord: "x",
    command: "changes.revert",
    tab: ["changes"],
    not: ["panelFiltering"],
    section: "the changes tab",
    desc: "revert this file — ⏎ confirms",
  },
  {
    mode: "panel",
    chord: "X",
    command: "changes.revertAll",
    tab: ["changes"],
    not: ["panelFiltering"],
    section: "the changes tab",
    desc: "revert everything — ⏎ confirms",
  },

  // -- one job's output -----------------------------------------------------
  // Documented as its own section: it is reached with ⏎ from the rail, and every
  // key here is one the rail or the transcript already means the same thing by.
  {
    mode: "job",
    chord: "esc",
    command: "job.close",
    section: "a background job (⏎ on a rail row)",
    desc: "back to the rail",
  },
  { mode: "job", chord: "q", command: "job.close" },
  { mode: "job", chord: "left", command: "job.close" },
  {
    mode: "job",
    chord: "up",
    command: "scroll.up",
    section: "a background job (⏎ on a rail row)",
    label: "↑/↓",
    desc: "scroll the output",
  },
  { mode: "job", chord: "down", command: "scroll.down" },
  { mode: "job", chord: "k", command: "scroll.up" },
  { mode: "job", chord: "j", command: "scroll.down" },
  {
    mode: "job",
    chord: "pageup",
    command: "scroll.pageUp",
    section: "a background job (⏎ on a rail row)",
    label: "pgup/pgdn",
    desc: "a screenful",
  },
  { mode: "job", chord: "pagedown", command: "scroll.pageDown" },
  {
    mode: "job",
    chord: "x",
    command: "job.stop",
    section: "a background job (⏎ on a rail row)",
    label: "x x",
    desc: "kill this job",
  },

  // -- the overlay itself ---------------------------------------------------
  { mode: "help", chord: "esc", command: "help.close" },
  { mode: "help", chord: "?", command: "help.close" },
  { mode: "help", chord: "q", command: "help.close" },
  { mode: "help", chord: "up", command: "scroll.up" },
  { mode: "help", chord: "down", command: "scroll.down" },
  { mode: "help", chord: "k", command: "scroll.up" },
  { mode: "help", chord: "j", command: "scroll.down" },
  // The overlay is 50-odd rows in a 24-row window and ↑↓ move three at a time, so
  // the last section — `won't do`, which is where the no-sandbox posture is stated —
  // was forty keypresses away. It already advertises pgup/pgdn for the transcript.
  { mode: "help", chord: "pageup", command: "scroll.pageUp" },
  { mode: "help", chord: "pagedown", command: "scroll.pageDown" },
];

function guardsHold(binding: Binding, ctx: KeyContext): boolean {
  for (const g of binding.when ?? []) if (!ctx[g]) return false;
  for (const g of binding.not ?? []) if (ctx[g]) return false;
  // A tab-scoped row is dead outside its tabs — including with the panel closed,
  // where `ctx.tab` is null and no tab-local letter can be meant.
  if (binding.tab && (!ctx.tab || !binding.tab.includes(ctx.tab))) return false;
  return true;
}

const modesOverlap = (a: Binding["mode"], b: Binding["mode"]) => a === b || a === "*" || b === "*";

/** The command a chord means in this context, or null when nothing is bound. */
export function lookup(ctx: KeyContext, chord: string): Command | null {
  if (chord === "") return null;
  for (const b of BINDINGS) {
    if (!modesOverlap(b.mode, ctx.mode) || b.chord !== chord) continue;
    if (guardsHold(b, ctx)) return b.command;
  }
  return null;
}

/** `lookup` straight off an ink keypress. The one entry point a component needs. */
export function resolve(ctx: KeyContext, input: string, key: KeyFlags = {}): Command | null {
  return lookup(ctx, chordOf(input, key));
}

// ---------------------------------------------------------------------------
// The help overlay, generated from the table
// ---------------------------------------------------------------------------

export interface HelpSection {
  section: string;
  keys: [string, string][];
  /** Prose rows with no key column. */
  limits?: boolean;
  /** Chords a terminal veteran will try that bough does not bind. */
  unavailable?: boolean;
  /** `/name` rows rather than chords — the same commands the `/` popup lists. */
  commands?: boolean;
}

/**
 * Things bough deliberately WON'T do, so a user stops waiting for them. Prose, no
 * key column — these are not bindings and must not be printed as if they were.
 */
export const LIMITS: HelpSection = {
  section: "won't do",
  limits: true,
  keys: [
    ["", "^c ^c quits; subagents keep running"],
    ["", "programs run as you — no sandbox"],
    ["", "changes land in your checkout as they happen"],
    ["", "a running workflow takes no input — stop, edit, relaunch"],
  ],
};

/**
 * Chords a terminal veteran WILL try that bough does not bind. Rendered muted,
 * never accented: silently eating ^r/^y/^z reads as broken, and printing them
 * like live keys is worse.
 */
export const UNAVAILABLE: HelpSection = {
  section: "not bound",
  unavailable: true,
  keys: [
    // Not a chord, but it belongs here for exactly the reason the section exists:
    // three of the four sigils a user is told to expect are live, and typing `!ls`
    // did not fail loudly — it went to the frontier model as an ordinary prompt and
    // billed for it. A sigil that is silently not a sigil is the one case where
    // saying nothing costs money.
    ["^g", "no $EDITOR handoff yet"],
    ["^v", "your terminal pastes · no image attachments"],
    // The two halves of "search" are different questions and this line answered
    // neither: ^r is still not bound, but saying only that left the reader believing
    // bough cannot search at all, when ^f then / searches every message.
    ["^r", "no reverse search · ^f then / searches every message"],
    ["^z", "no suspend · ^c ^c quits"],
    ["⌥d", "use ^k"],
    // NOT bough's choice: OpenTUI's React key layer delivers pgup/pgdn and drops these
    // two entirely — instrumented `useKeyboard` and saw `{"name":"pageup"}` arrive while
    // End and Home produced no event at all. `chordOf` even has `key.home`/`key.end`
    // branches, unreachable, waiting for the day they land. Listed here because a reader
    // WILL press End after scrolling back, and "nothing happens" needs a reason.
    ["home end", "not delivered by the terminal layer · use pgup/pgdn"],
  ],
};

/**
 * The overlay's sections, in table order.
 *
 * Derived, never authored: a key appears here because it is bound, with the text
 * the binding carries. That is the whole reason the descriptions live on the rows.
 */
export function helpSections(bindings: Binding[] = BINDINGS): HelpSection[] {
  const out: HelpSection[] = [];
  const bySection = new Map<string, [string, string][]>();
  for (const b of bindings) {
    if (!b.section || !b.desc) continue;
    let rows = bySection.get(b.section);
    if (!rows) {
      rows = [];
      bySection.set(b.section, rows);
      out.push({ section: b.section, keys: rows });
    }
    // A GUARDED row must say it is guarded. `^d` is printed twice — once as "what
    // this session changed" and once as "delete char ahead" — with nothing to say
    // they are one key that reads the composer to decide, and the failure direction
    // is silent: press `^d` with a half-written draft and the cursor at its end and
    // absolutely nothing happens. The condition already lives on the binding, so the
    // generator is the one place that can disclose it without inventing a second
    // description to keep in sync.
    // A `not` guard is disclosed for the same reason a `when` guard is. `esc esc` prints
    // TWICE — "clear the draft" and "go back to a turn and fork it · empty draft" — and
    // only the second said which state it belonged to, so the pair read as a
    // contradiction on the one screen that exists to answer what a key does.
    const desc = b.when?.includes("emptyDraft")
      ? `${b.desc} · empty draft`
      : b.not?.includes("emptyDraft")
      ? `${b.desc} · with a draft`
      : b.desc;
    rows.push([b.label ?? chordLabel(b.chord), desc]);
  }
  // The `/` commands, listed BY NAME. The overlay is generated from chords, so a
  // command with no chord — `/compact`, and any future one — appeared in the `/` popup
  // and nowhere else: `?` is the discoverability surface, and it was answering "every
  // key" when the user's question is "everything I can do".
  out.push({
    section: "typed at the prompt",
    commands: true,
    keys: [
      ["!cmd", "run it in your shell — not a message, not billed; output in the rail"],
      ["@path", "complete a file or directory into the message"],
      ...SLASH_COMMANDS.map((c) => [`/${c.name}`, c.desc] as [string, string]),
    ],
  });
  // WHAT THE TREE'S MARKS MEAN. Six glyphs decide how every row in the switcher reads —
  // and they were documented nowhere: not here, not in the keymap, not in the spec. A
  // user who sees `⑂` or `↦` had no way to find out what it was.
  out.push({
    section: "marks in the tree",
    commands: true,
    keys: [
      ["●", "a conversation you started"],
      ["↦", "a fresh conversation handed off from another"],
      ["⑂", "a fork — the same thread, cut at one turn"],
      ["≣", "a compaction — a span replaced by a summary"],
      ["◆ ◈", "a subagent · a workflow agent"],
      ["⋯ ✓ ✗ ◼", "running · finished · failed · stopped by a restart"],
    ],
  });
  out.push(LIMITS, UNAVAILABLE);
  return out;
}

/**
 * One PHYSICAL row of the overlay.
 *
 * The overlay is taller than any terminal it will ever be opened in — 50-odd rows
 * against a 24-row window — so it has to be a window over a list, and the list has
 * to exist as data before a component sees it. It did not, and the cost was the
 * bug this type exists to prevent: `Help` nested a `<Box>` per section inside a
 * parent pinned to `height={rows}`, yoga shrank the overflow away, and EVERY
 * section header plus one row per section was silently destroyed. `?` — the only
 * discoverability surface bough has — rendered as garbage on a default terminal,
 * and no test caught it because every test asserted `helpSections()` and none
 * asserted a rendered line.
 */
export interface HelpLine {
  kind: "header" | "row" | "blank";
  chord: string;
  desc: string;
  /** Rendered muted: the `won't do` prose and the `not bound` chords. */
  muted?: boolean;
  /** Prose rows carry a bullet instead of a key column. */
  prose?: boolean;
}

/**
 * The overlay as a flat list of rows, one per line the terminal will draw.
 *
 * Flattening is the whole point: `visible` can then be a slice, and a slice cannot
 * lose a header the way a squashed flexbox can.
 */
export function helpLines(sections: HelpSection[] = helpSections()): HelpLine[] {
  const out: HelpLine[] = [];
  for (const s of sections) {
    if (out.length > 0) out.push({ kind: "blank", chord: "", desc: "" });
    out.push({ kind: "header", chord: "", desc: s.section, muted: s.unavailable });
    for (const [chord, desc] of s.keys) {
      out.push({ kind: "row", chord, desc, muted: s.unavailable || s.limits, prose: s.limits });
    }
  }
  return out;
}

/**
 * Bindings that can never fire, as `"mode chord"` strings.
 *
 * Two rows match the same keypress when they share a mode and chord AND the
 * earlier one's guards are implied by the later one's — the simple cases being
 * identical guards, or an unguarded row placed ahead of a guarded one. Exported so
 * the test asserting the keymap has no dead rows reads as one call.
 *
 * TAB SCOPE COUNTS THE SAME WAY. Two rows that share a chord but name disjoint tabs
 * are the design (`x` stops a run, and reverts a file), so the subset test runs over
 * the tab sets too: an unscoped row ahead of a scoped one still kills it, and two
 * scoped rows only collide where their tabs overlap.
 */
export function deadBindings(bindings: Binding[] = BINDINGS): string[] {
  const dead: string[] = [];
  const sig = (b: Binding) =>
    `${[...(b.when ?? [])].sort().join(",")}/${[...(b.not ?? [])].sort().join(",")}${
      b.tab ? `@${[...b.tab].sort().join(",")}` : ""
    }`;
  for (let i = 0; i < bindings.length; i++) {
    for (let j = i + 1; j < bindings.length; j++) {
      const a = bindings[i];
      const b = bindings[j];
      if (!modesOverlap(a.mode, b.mode) || a.chord !== b.chord) continue;
      const aWhen = new Set(a.when ?? []);
      const aNot = new Set(a.not ?? []);
      // `a` shadows `b` when every context `b` accepts is one `a` also accepts —
      // i.e. `a`'s guards are a subset of `b`'s, and `a`'s tabs a superset.
      const tabShadows = a.tab === undefined ||
        (b.tab !== undefined && b.tab.every((t) => a.tab!.includes(t)));
      const shadows = tabShadows &&
        [...aWhen].every((g) => (b.when ?? []).includes(g)) &&
        [...aNot].every((g) => (b.not ?? []).includes(g));
      if (shadows) dead.push(`${b.mode} ${b.chord}${sig(b) === "/" ? "" : ` (${sig(b)})`}`);
    }
  }
  return dead;
}

// ---------------------------------------------------------------------------
// Line editing (pure)
// ---------------------------------------------------------------------------

export interface LineState {
  text: string;
  cursor: number;
}

export const EMPTY_LINE: LineState = { text: "", cursor: 0 };

const clamp = (text: string, cursor: number): LineState => ({
  text,
  cursor: Math.max(0, Math.min(cursor, text.length)),
});

/** Start of the logical line the cursor sits on. Multiline-aware, like ⌘←. */
function lineStart(text: string, cursor: number): number {
  const nl = text.lastIndexOf("\n", cursor - 1);
  return nl < 0 ? 0 : nl + 1;
}

function lineEnd(text: string, cursor: number): number {
  const nl = text.indexOf("\n", cursor);
  return nl < 0 ? text.length : nl;
}

/** Move the cursor one visual line, keeping its column where it can. */
function moveLine(s: LineState, dir: -1 | 1): LineState {
  const start = lineStart(s.text, s.cursor);
  const col = s.cursor - start;
  if (dir === -1) {
    if (start === 0) return s;
    const prevStart = lineStart(s.text, start - 1);
    return clamp(s.text, Math.min(prevStart + col, start - 1));
  }
  const end = lineEnd(s.text, s.cursor);
  if (end >= s.text.length) return s;
  const nextEnd = lineEnd(s.text, end + 1);
  return clamp(s.text, Math.min(end + 1 + col, nextEnd));
}

/**
 * Apply an editing command. Returns the SAME object when nothing changed, so a
 * component can skip a render on a no-op (backspace at column 0, ↑ on line one).
 */
export function editLine(s: LineState, command: Command): LineState {
  switch (command) {
    case "cursor.left":
      return s.cursor === 0 ? s : clamp(s.text, s.cursor - 1);
    case "cursor.right":
      return s.cursor >= s.text.length ? s : clamp(s.text, s.cursor + 1);
    case "cursor.home":
      return clamp(s.text, lineStart(s.text, s.cursor));
    case "cursor.end":
      return clamp(s.text, lineEnd(s.text, s.cursor));
    case "cursor.wordLeft":
      return clamp(s.text, wordLeft(s.text, s.cursor));
    case "cursor.wordRight":
      return clamp(s.text, wordRight(s.text, s.cursor));
    case "cursor.up":
      return moveLine(s, -1);
    case "cursor.down":
      return moveLine(s, 1);

    case "delete.back":
      return s.cursor === 0
        ? s
        : { text: s.text.slice(0, s.cursor - 1) + s.text.slice(s.cursor), cursor: s.cursor - 1 };
    case "delete.forward":
      return s.cursor >= s.text.length
        ? s
        : { text: s.text.slice(0, s.cursor) + s.text.slice(s.cursor + 1), cursor: s.cursor };
    case "delete.wordBack": {
      const from = wordLeft(s.text, s.cursor);
      return from === s.cursor
        ? s
        : { text: s.text.slice(0, from) + s.text.slice(s.cursor), cursor: from };
    }
    case "delete.toEnd": {
      const end = lineEnd(s.text, s.cursor);
      return end === s.cursor
        ? s
        : { text: s.text.slice(0, s.cursor) + s.text.slice(end), cursor: s.cursor };
    }
    case "delete.toStart": {
      const start = lineStart(s.text, s.cursor);
      return start === s.cursor
        ? s
        : { text: s.text.slice(0, start) + s.text.slice(s.cursor), cursor: start };
    }
    case "delete.line":
      return s.text === "" ? s : EMPTY_LINE;
    case "newline":
      return insertText(s, "\n");

    default:
      return s;
  }
}

/** Insert text at the cursor. The one mutation a keypress that is not a chord makes. */
export function insertText(s: LineState, text: string): LineState {
  if (text === "") return s;
  return {
    text: s.text.slice(0, s.cursor) + text + s.text.slice(s.cursor),
    cursor: s.cursor + text.length,
  };
}

/**
 * Invisible control bytes must never reach the draft — or the transcript.
 *
 * WHOLE SEQUENCES, not just the escape byte. Dropping the `\x1b` alone leaves the
 * rest of the sequence as ordinary printable characters, so a terminal emitting a
 * key bough does not decode types its encoding into the user's message:
 *
 *   › and then say done[27;3;13~
 *
 * — that is Alt+Enter under the kitty/modifyOtherKeys encoding, landing as text in
 * a half-written prompt. Any unrecognized CSI, SS3 or OSC does the same, and the
 * set of sequences a terminal can send is not one this app gets to enumerate. A
 * sequence is never something the user typed, so it goes whole or not at all.
 *
 * `strip-ansi` is already a dependency and already the repo's answer to "what is
 * an escape sequence" (`format.ts` measures with it), so it is the answer here too.
 */
export function stripCtl(s: string): string {
  // SS3 (`ESC O <char>` — F1-F4 and the application-mode arrows) first, because
  // `strip-ansi` covers CSI/OSC and not this one, and leaving it to the control-byte
  // pass would drop the ESC and keep the "P".
  // deno-lint-ignore no-control-regex -- stripping escape sequences is the point
  const noSs3 = s.replace(/\x1bO[\x20-\x7e]/g, "");
  // Then anything else introduced by an escape byte: a two-character sequence is
  // still a sequence, and its payload is still not something the user typed.
  // deno-lint-ignore no-control-regex -- as above
  const noEsc = stripAnsi(noSs3).replace(/\x1b[\x20-\x7e]/g, "");
  // deno-lint-ignore no-control-regex -- stripping control bytes is the point
  return noEsc.replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "");
}

/**
 * What a coalesced stdin chunk means for the composer.
 *
 * A fast typist's keystrokes and their Return arrive in ONE read, so a newline can
 * be data rather than a keypress. Only a trailing `\r` means "…then send": a bare
 * `\n` can only have come from ^j and is always a literal newline. The old tree
 * shipped the other rule and sent half-written messages.
 */
export function chunkInput(chunk: string): { body: string; send: boolean } {
  const send = chunk.endsWith("\r");
  const body = stripCtl((send ? chunk.slice(0, -1) : chunk).replace(/\r\n?/g, "\n"));
  return { body, send };
}

/** Is this keypress ordinary text rather than a chord? */
export function isTextInput(input: string, key: KeyFlags = {}): boolean {
  if (input === "") return false;
  if (key.ctrl || key.meta || key.super) return false;
  if (key.return || key.escape || key.tab || key.backspace || key.delete) return false;
  if (key.upArrow || key.downArrow || key.leftArrow || key.rightArrow) return false;
  if (key.pageUp || key.pageDown || key.home || key.end) return false;
  return true;
}
