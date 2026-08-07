//! ONE tree, for everything: conversations, the turns inside them, and what
//! branched off which turn (port of `src/tui/forest.ts`).
//!
//! THE RULES, all three derived and none of them stored:
//!
//! 1. **A conversation appears exactly once, under what it branched from.** A
//!    fork hangs off the MESSAGE it cut from. A session with no origin is a
//!    root and sits at the top level.
//! 2. **Turns are shown for expanded conversations only.** The top level is
//!    also the switcher — it has to stay scannable.
//! 3. **Spawned work still collapses into a count.** One row that says
//!    `⋯ 40 spawned` is both reachable and countable.
//!
//! PURE. Rows in, rows out — no fetch, no clock, no renderer. `seen` is a
//! CYCLE GUARD, not a dedupe: `originId` is a pointer the server sets and not
//! a foreign key, so a malformed lineage must render a short tree rather than
//! hang the terminal in an infinite walk.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use bough_core::schema::parts::{
    is_collapsed_kind, is_delegated_kind, Message, Part, Role, SessionKind, TurnStatus,
};

use crate::api::SessionRow;

/// True when this kind is only reachable under its origin.
pub fn is_collapsed(kind: SessionKind) -> bool {
    is_collapsed_kind(kind)
}

/// True when a program asked for this session inside a turn.
pub fn is_delegated(kind: SessionKind) -> bool {
    is_delegated_kind(kind)
}

/// One topic header, as `POST /sessions/:id/sections` returns it: an index
/// range over that session's OWN turns.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct SectionRange {
    pub start: usize,
    pub end: usize,
    pub label: String,
}

/// One rendered row.
///
/// `depth` is the indent and `id` is what a keypress acts on — unique across
/// kinds by construction, since a message id and a session id never collide.
#[derive(Clone, Debug)]
pub enum ForestRow {
    Session {
        id: String,
        session: SessionRow,
        depth: usize,
        /// Its turns are shown.
        open: bool,
        /// Children that collapse under this one, shown or not.
        delegated: usize,
        /// This is the conversation currently on screen.
        current: bool,
        /// Conversations BELOW this one that are running right now — delegated
        /// or branched, at any depth. A row that hides live work has to say so.
        busy_below: usize,
        /// It has turns to show (or might — an unfetched thread is not "empty").
        expandable: bool,
    },
    /// A TOPIC HEADER over the turns beneath it. Not selectable in any
    /// meaningful sense — but it IS a row, so the window math counts it.
    Section {
        id: String,
        session_id: String,
        depth: usize,
        label: String,
    },
    Message {
        id: String,
        /// Which conversation this turn belongs to — a fork needs the owner.
        session_id: String,
        depth: usize,
        role: Role,
        gist: String,
        /// The `/` filter's search matched this turn's own text.
        matched: bool,
        /// The thread's last message: where the next turn would append.
        active: bool,
        /// Drawn with `└─` rather than `├─`.
        last: bool,
        /// When it was said, for the right-hand age column.
        created_at: i64,
        /// Tool calls folded into this turn's `▸ n tools` chip.
        tools: usize,
        /// Those calls are shown as their own rows beneath it.
        tools_open: bool,
        /// Conversations that branched off THIS turn.
        branches: usize,
        /// It sits on the path from a root to the open conversation — the green
        /// trunk. Everything else is a sibling the eye should slide past.
        on_path: bool,
        /// The open conversation's last turn: where the next one appends.
        leaf: bool,
    },
    /// One tool call of an expanded turn.
    Tool {
        id: String,
        session_id: String,
        depth: usize,
        /// `read`, `edit`, `bash` — what the call DID, not the tool's name.
        verb: String,
        detail: String,
    },
    /// A conversation that branched off the turn above it, as ONE row.
    ///
    /// This is the flat trunk's whole trick: a branch never nests. The one the
    /// open conversation lives on is `active` and its turns continue at the
    /// TRUNK column right below; every sibling stays a single collapsed row.
    Branch {
        id: String,
        session: SessionRow,
        depth: usize,
        /// Its turns are on screen, continuing the trunk.
        active: bool,
        /// Drawn with `└` rather than `├`.
        last: bool,
        /// Turns under it — `None` when its thread has not been fetched, which
        /// is not the same as a branch with nothing in it.
        entries: Option<usize>,
        /// Branch points of its own, so a collapsed row still says it forks.
        forks: usize,
        /// Conversations running BELOW it. One row is all a branch gets, so
        /// that row is the only place live work under it can be admitted.
        busy_below: usize,
    },
    /// The collapsed fan-out: reachable, countable, one row.
    Collapsed {
        id: String,
        origin_id: String,
        depth: usize,
        count: usize,
    },
}

impl ForestRow {
    pub fn id(&self) -> &str {
        match self {
            ForestRow::Session { id, .. }
            | ForestRow::Section { id, .. }
            | ForestRow::Message { id, .. }
            | ForestRow::Tool { id, .. }
            | ForestRow::Branch { id, .. }
            | ForestRow::Collapsed { id, .. } => id,
        }
    }

    pub fn depth(&self) -> usize {
        match self {
            ForestRow::Session { depth, .. }
            | ForestRow::Section { depth, .. }
            | ForestRow::Message { depth, .. }
            | ForestRow::Tool { depth, .. }
            | ForestRow::Branch { depth, .. }
            | ForestRow::Collapsed { depth, .. } => *depth,
        }
    }
}

/// What a tool call DID, in two columns: the verb and its object.
///
/// bough grants exactly one tool — `run_steps` — so the tool's NAME says
/// nothing; the program inside it does. [`crate::lines::program_summary`] is
/// the same derivation the transcript header uses, so a call reads identically
/// in both places, and a program it does not recognise falls back to its first
/// meaningful line rather than to an empty row.
fn tool_row(part: &Part, max: usize) -> Option<(String, String)> {
    let Part::ToolCall { name, input, .. } = part else {
        return None;
    };
    let code = input.get("code").and_then(|c| c.as_str()).unwrap_or("");
    let summary = crate::lines::program_summary(code, max, false);
    let detail = if summary.is_empty() {
        crate::lines::code_gist(input, max)
    } else {
        summary
    };
    Some((name.clone(), detail))
}

/// A message's text parts, VERBATIM — newlines, runs of spaces and all.
///
/// The counterpart to [`message_gist`], and deliberately not it: handing a
/// message back to the composer must not collapse whitespace, or taking back a
/// three-paragraph message returns it as one long line.
pub fn message_text(m: &Message) -> String {
    m.parts
        .iter()
        .map(|p| match p {
            Part::Text { text } => text.as_str(),
            _ => "",
        })
        .collect()
}

/// The visible text of a message, collapsed to one line.
pub fn message_gist(m: &Message, max: usize) -> String {
    let joined: Vec<&str> = m
        .parts
        .iter()
        .map(|p| match p {
            Part::Text { text } => text.as_str(),
            _ => "",
        })
        .collect();
    let text = collapse_ws(&joined.join(" "));
    if text.is_empty() {
        // A turn that is only tool calls still needs a name — it is a real
        // node, and the whole point of the tree is that you can go back to it.
        let calls = m
            .parts
            .iter()
            .filter(|p| matches!(p, Part::ToolCall { .. }))
            .count();
        return if calls > 0 {
            format!("({calls} step{})", if calls == 1 { "" } else { "s" })
        } else {
            "(no text)".to_string()
        };
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > max {
        let head: String = chars[..max.saturating_sub(1)].iter().collect();
        format!("{}…", head.trim_end())
    } else {
        text
    }
}

/// `\s+` → one space, then trim (the JS regex, including every Unicode space).
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            in_ws = true;
            continue;
        }
        if in_ws && !out.is_empty() {
            out.push(' ');
        }
        in_ws = false;
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

static NO_CHILDREN: LazyLock<HashMap<String, Vec<SessionRow>>> = LazyLock::new(HashMap::new);
static NO_THREADS: LazyLock<HashMap<String, Vec<Message>>> = LazyLock::new(HashMap::new);
static NO_IDS: LazyLock<HashSet<String>> = LazyLock::new(HashSet::new);

/// Everything the walk reads. Absent is never the same as empty: an unfetched
/// thread MIGHT have turns, and a fetched-empty one does not.
#[derive(Clone)]
pub struct ForestInput<'a> {
    /// `GET /sessions` — every session that does not collapse.
    pub sessions: &'a [SessionRow],
    /// `originId` → `GET /sessions?originId=`. Absent means "not fetched yet".
    pub children_by_origin: &'a HashMap<String, Vec<SessionRow>>,
    /// Session id → its thread. Absent = not fetched, which is NOT empty.
    pub threads: &'a HashMap<String, Vec<Message>>,
    /// Sessions whose turns are shown.
    pub expanded: &'a HashSet<String>,
    /// Sessions whose delegated fan-out is drilled into.
    pub drilled: &'a HashSet<String>,
    /// Message ids whose tool calls are shown as rows.
    pub tools_open: &'a HashSet<String>,
    /// View the tree from THIS conversation down — 2b's re-rooting. Its own
    /// row is not drawn (the breadcrumb says where you are); its turns start at
    /// the trunk. `None` = the whole forest, from the roots.
    pub root_id: Option<&'a str>,
    /// Topic sections per session. Absent = no headers, not "no topics".
    pub sections: Option<&'a HashMap<String, Vec<SectionRange>>>,
    /// The conversation on screen, marked and never filtered out.
    pub current_id: Option<&'a str>,
    /// Narrows the TOP LEVEL. A branch is never hidden from its parent.
    pub filter: Option<&'a str>,
    /// Session ids whose MESSAGES matched, from `GET /search`.
    pub matched_sessions: &'a [String],
    /// Message ids the search matched, so the row that said the word is marked.
    pub matched_messages: &'a [String],
    /// Show only user turns.
    pub user_only: bool,
}

impl Default for ForestInput<'_> {
    fn default() -> Self {
        ForestInput {
            sessions: &[],
            children_by_origin: &NO_CHILDREN,
            threads: &NO_THREADS,
            expanded: &NO_IDS,
            drilled: &NO_IDS,
            tools_open: &NO_IDS,
            root_id: None,
            sections: None,
            current_id: None,
            filter: None,
            matched_sessions: &[],
            matched_messages: &[],
            user_only: false,
        }
    }
}

struct Walk<'a> {
    input: &'a ForestInput<'a>,
    rows: Vec<ForestRow>,
    seen: HashSet<String>,
    /// The open conversation and every origin above it. THE TRUNK: these are
    /// the branches whose turns continue at column zero, and the guides that
    /// are drawn in the active colour.
    path: HashSet<String>,
}

impl Walk<'_> {
    /// Every known child of `id`, from both sources, deduped by id and in
    /// insertion order (the JS `Map` order the sorts below rely on).
    fn children_of(&self, id: &str) -> Vec<SessionRow> {
        let mut out: Vec<SessionRow> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut push = |s: &SessionRow, out: &mut Vec<SessionRow>| match index.get(&s.session.id) {
            Some(&at) => out[at] = s.clone(),
            None => {
                index.insert(s.session.id.clone(), out.len());
                out.push(s.clone());
            }
        };
        for s in self.input.sessions {
            if s.session.origin_id.as_deref() == Some(id) {
                push(s, &mut out);
            }
        }
        if let Some(list) = self.input.children_by_origin.get(id) {
            for s in list {
                push(s, &mut out);
            }
        }
        out
    }

    /// Running descendants at any depth. Cycle-guarded like the walk itself.
    fn busy_below(&self, id: &str, guard: &mut HashSet<String>) -> usize {
        if !guard.insert(id.to_string()) {
            return 0;
        }
        let mut n = 0;
        for c in self.children_of(id) {
            if c.busy || c.last_turn_status == Some(TurnStatus::Running) {
                n += 1;
            }
            n += self.busy_below(&c.session.id, guard);
        }
        n
    }

    /// This session's children that are branches, oldest first.
    fn branches_of(&self, id: &str) -> Vec<SessionRow> {
        let mut out: Vec<SessionRow> = self
            .children_of(id)
            .into_iter()
            .filter(|c| !is_collapsed(c.session.kind))
            .collect();
        out.sort_by_key(|c| c.session.created_at);
        out
    }

    /// Which of these siblings continues the trunk.
    ///
    /// The one the open conversation is on, if any — otherwise the NEWEST,
    /// because a branch point you have never returned to is one you left by
    /// making the last branch. Exactly one sibling expands, always: two would
    /// reintroduce the diagonal this layout exists to remove.
    fn active_branch(&self, siblings: &[SessionRow]) -> Option<String> {
        siblings
            .iter()
            .find(|s| self.path.contains(&s.session.id))
            .or_else(|| siblings.last())
            .map(|s| s.session.id.clone())
    }

    fn walk(&mut self, session: &SessionRow, depth: usize) {
        if !self.seen.insert(session.session.id.clone()) {
            return;
        }
        let id = session.session.id.clone();
        let children = self.children_of(&id);
        // COLLAPSE, not delegation: a schedule firing hangs under the
        // conversation that created the schedule exactly as a subagent hangs
        // under its spawner, even though nothing delegated it.
        let branches = self.branches_of(&id);
        let mut delegated: Vec<SessionRow> = children
            .iter()
            .filter(|c| is_collapsed(c.session.kind))
            .cloned()
            .collect();
        delegated.sort_by_key(|c| c.session.created_at);
        let thread = self.input.threads.get(&id);
        let open = self.input.expanded.contains(&id);
        self.rows.push(ForestRow::Session {
            id: id.clone(),
            session: session.clone(),
            depth,
            open,
            delegated: delegated.len(),
            current: self.input.current_id == Some(id.as_str()),
            busy_below: self.busy_below(&id, &mut HashSet::new()),
            // A conversation with a fetched-and-empty thread and no branches
            // has nothing under it; one whose thread has not been fetched
            // MIGHT, and rendering it as a leaf would be a claim the caller
            // has not made yet.
            expandable: thread.is_none()
                || thread.is_some_and(|t| !t.is_empty())
                || !branches.is_empty()
                || !delegated.is_empty(),
        });
        if !open {
            return;
        }
        self.trunk(session, depth);
    }

    /// This conversation's turns, AT THE TRUNK COLUMN, and the branches that
    /// cut from them.
    ///
    /// THE ONE RULE THIS FILE IS ABOUT: `depth` does not grow as the
    /// conversation does. A linear turn sits where the turn above it sat, no
    /// matter how many forks deep the reader has walked, so a long
    /// conversation stays a column instead of drifting off the right edge one
    /// character per branch. Only the branch ROWS take `depth + 1`, and the one
    /// that continues the trunk drops straight back to `depth`.
    fn trunk(&mut self, session: &SessionRow, depth: usize) {
        let id = session.session.id.clone();
        let branches = self.branches_of(&id);
        let thread = self.input.threads.get(&id);
        let empty: Vec<Message> = Vec::new();
        let full = thread.unwrap_or(&empty);
        let shown: Vec<&Message> = if self.input.user_only {
            full.iter().filter(|m| m.role == Role::User).collect()
        } else {
            full.iter().collect()
        };
        let last_id = full.last().map(|m| m.id.clone());
        let on_path = self.path.contains(&id) || self.input.current_id == Some(id.as_str());
        let mut placed: HashSet<String> = HashSet::new();
        let no_sections: Vec<SectionRange> = Vec::new();
        let sections = self
            .input
            .sections
            .and_then(|m| m.get(&id))
            .unwrap_or(&no_sections);
        let shown_len = shown.len();
        for (i, m) in shown.iter().enumerate() {
            // A label with no letters is not a topic. The route really does
            // return them, and a header reading `── …` is worse than none.
            if let Some(head) = sections
                .iter()
                .find(|sec| sec.start == i && sec.label.chars().any(|c| c.is_alphabetic()))
            {
                self.rows.push(ForestRow::Section {
                    id: format!("section:{id}:{i}"),
                    session_id: id.clone(),
                    depth,
                    label: head.label.clone(),
                });
            }
            let under: Vec<SessionRow> = branches
                .iter()
                .filter(|b| b.session.origin_message_id.as_deref() == Some(m.id.as_str()))
                .cloned()
                .collect();
            let tools = m
                .parts
                .iter()
                .filter(|p| matches!(p, Part::ToolCall { .. }))
                .count();
            let tools_open = self.input.tools_open.contains(&m.id);
            self.rows.push(ForestRow::Message {
                id: m.id.clone(),
                session_id: id.clone(),
                depth,
                role: m.role,
                gist: message_gist(m, 56),
                matched: self.input.matched_messages.iter().any(|x| x == &m.id),
                active: Some(&m.id) == last_id.as_ref(),
                last: i + 1 == shown_len && under.is_empty(),
                created_at: m.created_at,
                tools,
                tools_open,
                branches: under.len(),
                on_path,
                // The LEAF is where the next turn appends, so it is a fact
                // about the OPEN conversation and no other: marking the last
                // turn of every expanded thread would put four "you are here"
                // arrows on one screen.
                leaf: Some(&m.id) == last_id.as_ref() && self.input.current_id == Some(id.as_str()),
            });
            if tools_open {
                for (n, part) in m.parts.iter().enumerate() {
                    if let Some((verb, detail)) = tool_row(part, 46) {
                        self.rows.push(ForestRow::Tool {
                            id: format!("tool:{}:{n}", m.id),
                            session_id: id.clone(),
                            depth,
                            verb,
                            detail,
                        });
                    }
                }
            }
            for b in under.iter() {
                placed.insert(b.session.id.clone());
            }
            self.fan_out(&under, depth);
        }
        // A branch whose origin turn is not in this thread (a compaction
        // dropped it, or the branch cut from an ancestor) still has to be
        // reachable, so it falls through here rather than vanishing.
        let orphans: Vec<SessionRow> = branches
            .iter()
            .filter(|b| !placed.contains(&b.session.id))
            .cloned()
            .collect();
        self.fan_out(&orphans, depth);

        let mut delegated: Vec<SessionRow> = self
            .children_of(&id)
            .into_iter()
            .filter(|c| is_collapsed(c.session.kind))
            .collect();
        delegated.sort_by_key(|c| c.session.created_at);
        if delegated.is_empty() {
            return;
        }
        if self.input.drilled.contains(&id) {
            for child in delegated {
                self.walk(&child, depth + 1);
            }
        } else {
            self.rows.push(ForestRow::Collapsed {
                id: format!("collapsed:{id}"),
                origin_id: id.clone(),
                depth: depth + 1,
                count: delegated.len(),
            });
        }
    }

    /// The siblings of one branch point: a row each, and the active one's turns
    /// picked straight back up at the trunk.
    fn fan_out(&mut self, siblings: &[SessionRow], depth: usize) {
        if siblings.is_empty() {
            return;
        }
        let active = self.active_branch(siblings);
        let n = siblings.len();
        for (i, b) in siblings.iter().enumerate() {
            let bid = b.session.id.clone();
            let is_active = active.as_deref() == Some(bid.as_str());
            if !self.seen.insert(bid.clone()) {
                continue;
            }
            let kids = self.branches_of(&bid);
            self.rows.push(ForestRow::Branch {
                id: bid.clone(),
                session: b.clone(),
                depth: depth + 1,
                active: is_active,
                last: i + 1 == n,
                entries: self.input.threads.get(&bid).map(|t| t.len()),
                forks: kids.len(),
                busy_below: self.busy_below(&bid, &mut HashSet::new()),
            });
            if is_active {
                self.trunk(b, depth);
            }
        }
    }
}

/// Build the rows, depth-first.
pub fn forest_rows(input: &ForestInput) -> Vec<ForestRow> {
    // The trunk, before any row is built: which branch expands at each fork is
    // a question about the open conversation's ancestry, and the walk meets the
    // forks on its way DOWN.
    let mut path: HashSet<String> = HashSet::new();
    if let Some(current) = input.current_id {
        path.insert(current.to_string());
        for id in reveal_path(input.sessions, input.children_by_origin, Some(current)) {
            path.insert(id);
        }
    }
    let mut walk = Walk {
        input,
        rows: Vec::new(),
        seen: HashSet::new(),
        path,
    };
    // RE-ROOTED: one conversation's trunk, with no row of its own. The
    // breadcrumb above the list carries the lineage, so the depth that would
    // have shown it is spent on the turns instead.
    if let Some(root) = input.root_id {
        if let Some(s) = input
            .sessions
            .iter()
            .chain(input.children_by_origin.values().flatten())
            .find(|s| s.session.id == root)
            .cloned()
        {
            walk.seen.insert(root.to_string());
            walk.trunk(&s, 0);
            return walk.rows;
        }
    }
    let mut roots: Vec<SessionRow> = input
        .sessions
        .iter()
        .filter(|s| match &s.session.origin_id {
            None => true,
            Some(origin) => !input.sessions.iter().any(|o| &o.session.id == origin),
        })
        .filter(|s| matches(s, input.filter, input.current_id, input.matched_sessions))
        .cloned()
        .collect();
    // Newest first at the top level: this is also the switcher.
    roots.sort_by_key(|r| std::cmp::Reverse(r.session.created_at));
    for root in roots {
        walk.walk(&root, 0);
    }
    walk.rows
}

/// Does this top-level conversation survive the filter?
///
/// The open one always does. Narrowing the list you are looking at until the
/// conversation you are IN disappears from it is disorienting in a way no
/// filter should be, and it is the row the cursor most often wants back.
fn matches(
    s: &SessionRow,
    filter: Option<&str>,
    current_id: Option<&str>,
    matched: &[String],
) -> bool {
    let q = filter.unwrap_or("").trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    if Some(s.session.id.as_str()) == current_id {
        return true;
    }
    if matched.iter().any(|m| m == &s.session.id) {
        return true;
    }
    format!(
        "{} {}",
        s.session.title,
        s.session.workspace.as_deref().unwrap_or("")
    )
    .to_lowercase()
    .contains(&q)
}

/// The conversations that must be EXPANDED for `current_id` to be on screen —
/// its chain of origins, nearest last, excluding itself. Pure and
/// cycle-guarded, and it only SEEDS: a row the user then collapses stays
/// collapsed.
pub fn reveal_path(
    sessions: &[SessionRow],
    children_by_origin: &HashMap<String, Vec<SessionRow>>,
    current_id: Option<&str>,
) -> Vec<String> {
    let Some(current) = current_id else {
        return Vec::new();
    };
    if current.is_empty() {
        return Vec::new();
    }
    let mut by_id: HashMap<&str, &SessionRow> = HashMap::new();
    for s in sessions {
        by_id.insert(s.session.id.as_str(), s);
    }
    for list in children_by_origin.values() {
        for s in list {
            by_id.insert(s.session.id.as_str(), s);
        }
    }
    let mut path: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::from([current.to_string()]);
    let mut cur: Option<String> = by_id.get(current).and_then(|s| s.session.origin_id.clone());
    while let Some(id) = cur {
        if seen.contains(&id) {
            break;
        }
        seen.insert(id.clone());
        path.insert(0, id.clone());
        cur = by_id
            .get(id.as_str())
            .and_then(|s| s.session.origin_id.clone());
    }
    path
}

/// The row a cursor at `selected` is on, or None past the end.
pub fn row_at(rows: &[ForestRow], selected: usize) -> Option<&ForestRow> {
    rows.get(selected)
}

/// The index a rewind should land on: the open conversation's last USER turn,
/// falling back to its last turn and then to its own row.
pub fn rewind_index(rows: &[ForestRow], current_id: Option<&str>) -> usize {
    let Some(current) = current_id else { return 0 };
    let mut session: Option<usize> = None;
    let mut last_turn: Option<usize> = None;
    let mut last_user: Option<usize> = None;
    for (i, r) in rows.iter().enumerate() {
        match r {
            ForestRow::Session { id, .. } | ForestRow::Branch { id, .. } if id == current => {
                session = Some(i)
            }
            ForestRow::Message {
                session_id, role, ..
            } if session_id == current => {
                last_turn = Some(i);
                if *role == Role::User {
                    last_user = Some(i);
                }
            }
            _ => {}
        }
    }
    last_user.or(last_turn).or(session).unwrap_or(0)
}

/// What `Enter` on a row means — pi's selection rules, and bough's fork body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// Nothing to do — a caption row. Present so [`selection_for`] is total.
    None,
    Open(String),
    Expand(String),
    Drill(String),
    /// Show the tree FROM this conversation — 2b. Reading a sibling branch
    /// without joining it, and the reason nothing here has to nest.
    ReRoot(String),
    Fork {
        session_id: String,
        at_message_id: String,
        /// The caller intends to re-send it itself (a user turn cuts BEFORE it).
        exclusive: bool,
        editor_text: Option<String>,
    },
}

/// What taking back the last thing you sent has to act on. Newest first, and
/// queued before sent: the queue holds what was typed most recently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TakeBack {
    /// Pop the tail of the local queue. Nothing outside this client knew.
    Queued,
    /// Unsend this message — it and its partial answer go — and hand the text back.
    Sent { at_message_id: String, text: String },
    /// The window was armed but the message has not landed.
    None,
}

pub fn take_back_target(queued: &[String], thread: &[Message]) -> TakeBack {
    if !queued.is_empty() {
        return TakeBack::Queued;
    }
    // The LAST user turn, searched from the end rather than assumed to be the
    // final message: the supervisor's reply may already be streaming.
    match thread.iter().rev().find(|m| m.role == Role::User) {
        Some(m) => TakeBack::Sent {
            at_message_id: m.id.clone(),
            text: message_text(m),
        },
        None => TakeBack::None,
    }
}

pub fn selection_for(row: &ForestRow, threads: &HashMap<String, Vec<Message>>) -> Selection {
    match row {
        ForestRow::Collapsed { origin_id, .. } => Selection::Drill(origin_id.clone()),
        // A SECTION HEADER IS NOT A TURN: ⏎ on a caption must not ask the
        // server to fork at a message id that does not exist.
        // Nor is a tool call: it is a line of evidence under a turn.
        ForestRow::Section { .. } | ForestRow::Tool { .. } => Selection::None,
        // ⏎ on a conversation OPENS it — the switcher half of this surface
        // stays one keypress; walking into its turns is `→`.
        ForestRow::Session { id, .. } => Selection::Open(id.clone()),
        // A COLLAPSED sibling is somewhere you have not been: ⏎ re-roots the
        // view onto it so it can be read without joining it. The one already
        // carrying the trunk is somewhere you can only GO, so ⏎ goes.
        ForestRow::Branch { id, active, .. } => {
            if *active {
                Selection::Open(id.clone())
            } else {
                Selection::ReRoot(id.clone())
            }
        }
        ForestRow::Message { id, session_id, .. } => {
            let m = threads
                .get(session_id)
                .and_then(|t| t.iter().find(|x| &x.id == id));
            match m {
                Some(m) if m.role == Role::User => Selection::Fork {
                    session_id: session_id.clone(),
                    at_message_id: id.clone(),
                    exclusive: true,
                    editor_text: Some(message_gist(m, usize::MAX)),
                },
                _ => Selection::Fork {
                    session_id: session_id.clone(),
                    at_message_id: id.clone(),
                    exclusive: false,
                    editor_text: None,
                },
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fixtures shared with the tree tab's tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use bough_core::schema::parts::Session;

    pub fn session_row(id: &str, kind: SessionKind, created_at: i64) -> SessionRow {
        SessionRow {
            session: Session {
                id: id.to_string(),
                title: id.to_string(),
                kind,
                created_at,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: None,
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            },
            busy: false,
            last_turn_status: None,
            cost_usd: None,
            tokens: None,
        }
    }

    pub fn with_origin(mut s: SessionRow, origin: &str) -> SessionRow {
        s.session.origin_id = Some(origin.to_string());
        s
    }

    pub fn with_status(mut s: SessionRow, status: TurnStatus) -> SessionRow {
        s.last_turn_status = Some(status);
        s
    }

    pub fn msg(id: &str, role: Role, text: &str) -> Message {
        Message {
            id: id.to_string(),
            session_id: "s".into(),
            role,
            parts: if text.is_empty() {
                vec![]
            } else {
                vec![Part::Text {
                    text: text.to_string(),
                }]
            },
            pending: false,
            created_at: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/forest.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    /// Row ids, with a marker for the non-session kinds, so a shape reads at a
    /// glance (the TS suite's `shape`).
    fn shape(rows: &[ForestRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                ForestRow::Session { id, .. } => id.clone(),
                ForestRow::Message { id, .. } => format!("·{id}"),
                ForestRow::Section { label, .. } => format!("§{label}"),
                ForestRow::Collapsed { origin_id, .. } => format!("⋯{origin_id}"),
                ForestRow::Tool { id, .. } => format!("✦{id}"),
                // The one the trunk continues on is `>id`; a collapsed sibling
                // is `+id`. The shapes below are about exactly that difference.
                ForestRow::Branch { id, active, .. } => {
                    format!("{}{id}", if *active { ">" } else { "+" })
                }
            })
            .collect()
    }

    fn ids(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn thread() -> Vec<Message> {
        vec![
            msg("m1", Role::User, "add a discount function"),
            msg(
                "m2",
                Role::Supervisor,
                "done, it multiplies by (1 - pct/100)",
            ),
            msg("m3", Role::User, "now validate pct"),
        ]
    }

    #[test]
    fn collapsing_and_delegation_are_different_questions() {
        assert!(is_delegated(SessionKind::Subagent));
        assert!(is_delegated(SessionKind::WorkflowAgent));
        assert!(
            !is_delegated(SessionKind::ScheduleRun),
            "the clock is not a delegator"
        );
        assert!(!is_delegated(SessionKind::Root));
        assert!(!is_delegated(SessionKind::Fork));
        assert!(!is_delegated(SessionKind::Compaction));

        assert!(is_collapsed(SessionKind::Subagent));
        assert!(is_collapsed(SessionKind::WorkflowAgent));
        assert!(is_collapsed(SessionKind::ScheduleRun));
        assert!(!is_collapsed(SessionKind::Root));
        assert!(!is_collapsed(SessionKind::Fork));
        assert!(!is_collapsed(SessionKind::Compaction));
        // The conversation `!` runs in is the user's own, and openable.
        assert!(!is_collapsed(SessionKind::Shell));
        assert!(!is_delegated(SessionKind::Shell));
    }

    #[test]
    fn a_schedules_firings_collapse_under_the_conversation_that_made_it() {
        let creator = session_row("root", SessionKind::Root, 1);
        let runs = vec![
            with_origin(session_row("run-1", SessionKind::ScheduleRun, 2), "root"),
            with_origin(session_row("run-2", SessionKind::ScheduleRun, 3), "root"),
        ];
        let children: HashMap<String, Vec<SessionRow>> = HashMap::from([("root".into(), runs)]);
        let threads: HashMap<String, Vec<Message>> = HashMap::from([("root".into(), vec![])]);
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: std::slice::from_ref(&creator),
            children_by_origin: &children,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        // One row for the conversation, one for its runs — not a row per firing.
        assert_eq!(shape(&rows), vec!["root", "⋯root"]);
        match &rows[1] {
            ForestRow::Collapsed { count, .. } => assert_eq!(*count, 2),
            other => panic!("{other:?}"),
        }
        match &rows[0] {
            ForestRow::Session { delegated, .. } => {
                assert_eq!(*delegated, 2, "counted on the parent row")
            }
            other => panic!("{other:?}"),
        }
    }

    struct Fixture {
        sessions: Vec<SessionRow>,
        children: HashMap<String, Vec<SessionRow>>,
        threads: HashMap<String, Vec<Message>>,
    }

    /// One root, a fork of it cut at m1, a fan-out of three workflow agents
    /// under the fork, and a subagent under the root.
    fn fixture() -> Fixture {
        let root = session_row("root", SessionKind::Root, 1);
        let mut fork = with_origin(session_row("fork", SessionKind::Fork, 2), "root");
        fork.session.parent_id = Some("root".into());
        fork.session.origin_message_id = Some("m1".into());
        let agents: Vec<SessionRow> = ["w1", "w2", "w3"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                with_origin(
                    session_row(id, SessionKind::WorkflowAgent, 3 + i as i64),
                    "fork",
                )
            })
            .collect();
        let sub = with_origin(session_row("sub", SessionKind::Subagent, 10), "root");
        Fixture {
            sessions: vec![root, fork.clone()],
            children: HashMap::from([("root".into(), vec![fork, sub]), ("fork".into(), agents)]),
            threads: HashMap::from([("root".into(), thread())]),
        }
    }

    #[test]
    fn a_collapsed_conversation_is_one_row_the_top_level_stays_a_switcher() {
        let f = fixture();
        let rows = forest_rows(&ForestInput {
            sessions: &f.sessions,
            children_by_origin: &f.children,
            threads: &f.threads,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["root"]);
        match &rows[0] {
            ForestRow::Session { expandable, .. } => assert!(*expandable),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_fan_out_collapses_to_one_countable_row_and_drill_in_surfaces_just_that_one() {
        let f = fixture();
        let expanded = ids(&["root", "fork"]);
        let closed = forest_rows(&ForestInput {
            sessions: &f.sessions,
            children_by_origin: &f.children,
            threads: &f.threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert_eq!(
            shape(&closed),
            vec!["root", "·m1", ">fork", "⋯fork", "·m2", "·m3", "⋯root"]
        );

        let drilled = ids(&["fork"]);
        let opened = forest_rows(&ForestInput {
            sessions: &f.sessions,
            children_by_origin: &f.children,
            threads: &f.threads,
            expanded: &expanded,
            drilled: &drilled,
            ..Default::default()
        });
        assert_eq!(
            shape(&opened),
            vec!["root", "·m1", ">fork", "w1", "w2", "w3", "·m2", "·m3", "⋯root"]
        );
        // The root's own fan-out stays collapsed.
        assert!(shape(&opened).contains(&"⋯root".to_string()));
    }

    #[test]
    fn a_fork_is_drawn_once_under_the_turn_it_branched_from() {
        let f = fixture();
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &f.sessions,
            children_by_origin: &f.children,
            threads: &f.threads,
            expanded: &expanded,
            ..Default::default()
        });
        let forks: Vec<&ForestRow> = rows
            .iter()
            .filter(|r| matches!(r, ForestRow::Branch { id, .. } if id == "fork"))
            .collect();
        assert_eq!(forks.len(), 1);
        // THE WHOLE POINT: the turn it cut from is at the trunk (0) and the
        // branch is one column in — not two, and not one more per fork.
        assert_eq!(forks[0].depth(), 1);
        assert_eq!(rows.iter().map(ForestRow::depth).max(), Some(1));
        let top = forest_rows(&ForestInput {
            sessions: &f.sessions,
            children_by_origin: &f.children,
            threads: &f.threads,
            ..Default::default()
        });
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].id(), "root");
    }

    #[test]
    fn a_branch_whose_origin_turn_is_not_in_the_thread_is_still_reachable() {
        let root = session_row("root", SessionKind::Root, 1);
        let mut orphan = with_origin(session_row("orphan", SessionKind::Fork, 2), "root");
        orphan.session.origin_message_id = Some("gone".into());
        let sessions = vec![root, orphan.clone()];
        let children = HashMap::from([("root".to_string(), vec![orphan])]);
        let threads = HashMap::from([("root".to_string(), thread())]);
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            children_by_origin: &children,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["root", "·m1", "·m2", "·m3", ">orphan"]);
    }

    /// THE RULE THE WHOLE LAYOUT RESTS ON. Six forks deep, the last turn is at
    /// the same column as the first — otherwise a day's conversation walks off
    /// the right edge one character at a time.
    #[test]
    fn depth_never_exceeds_one_no_matter_how_many_forks_deep_the_path_runs() {
        let mut sessions = vec![session_row("s0", SessionKind::Root, 0)];
        let mut threads: HashMap<String, Vec<Message>> = HashMap::new();
        let mut expanded: HashSet<String> = HashSet::new();
        for i in 0..6 {
            let id = format!("s{i}");
            threads.insert(id.clone(), vec![msg(&format!("m{i}"), Role::User, "go")]);
            expanded.insert(id.clone());
            let mut next = with_origin(
                session_row(&format!("s{}", i + 1), SessionKind::Fork, i as i64 + 1),
                &id,
            );
            next.session.origin_message_id = Some(format!("m{i}"));
            sessions.push(next);
        }
        threads.insert("s6".into(), vec![msg("m6", Role::User, "go")]);
        expanded.insert("s6".into());
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            current_id: Some("s6"),
            ..Default::default()
        });
        assert_eq!(rows.iter().map(ForestRow::depth).max(), Some(1));
        // Every turn is at the trunk, and the branch rows are the only column.
        for r in &rows {
            if let ForestRow::Message { depth, .. } = r {
                assert_eq!(*depth, 0, "a turn left the trunk");
            }
        }
        // The path to the open conversation is the one that expanded — all six.
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, ForestRow::Message { .. }))
                .count(),
            7
        );
    }

    #[test]
    fn one_sibling_continues_the_trunk_and_the_rest_stay_one_row_each() {
        let root = session_row("root", SessionKind::Root, 1);
        let branch = |id: &str, at: i64| {
            let mut b = with_origin(session_row(id, SessionKind::Fork, at), "root");
            b.session.origin_message_id = Some("m1".into());
            b
        };
        let a = branch("a", 2);
        let b = branch("b", 3);
        let c = branch("c", 4);
        let sessions = vec![root, a.clone(), b.clone(), c.clone()];
        let threads = HashMap::from([
            ("root".to_string(), vec![msg("m1", Role::User, "refactor")]),
            (
                "b".to_string(),
                vec![msg("mb", Role::Supervisor, "indexed")],
            ),
        ]);
        let expanded = ids(&["root", "a", "b", "c"]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            // The open conversation is `b`, so `b` is the one that expands —
            // NOT the newest, which is `c`.
            current_id: Some("b"),
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["root", "·m1", "+a", ">b", "·mb", "+c"]);
        // The turn they cut from says how many ways it went.
        match &rows[1] {
            ForestRow::Message { branches, leaf, .. } => {
                assert_eq!(*branches, 3);
                assert!(!*leaf, "the branch point is not where the next turn lands");
            }
            other => panic!("{other:?}"),
        }
        // The leaf belongs to the OPEN conversation and to nothing else.
        let leaves: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                ForestRow::Message { id, leaf: true, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(leaves, vec!["mb"]);
        // A collapsed sibling still says how much is behind it.
        match rows.iter().find(|r| r.id() == "a").unwrap() {
            ForestRow::Branch {
                entries, active, ..
            } => {
                assert!(!*active);
                assert_eq!(*entries, None, "`a`'s thread was never fetched");
            }
            other => panic!("{other:?}"),
        }
    }

    /// With nowhere to be, the trunk follows the LAST branch made — the one the
    /// conversation was left on.
    #[test]
    fn with_no_open_conversation_the_newest_branch_carries_the_trunk() {
        let root = session_row("root", SessionKind::Root, 1);
        let mut old = with_origin(session_row("old", SessionKind::Fork, 2), "root");
        old.session.origin_message_id = Some("m1".into());
        let mut new = with_origin(session_row("new", SessionKind::Fork, 9), "root");
        new.session.origin_message_id = Some("m1".into());
        let sessions = vec![root, old, new];
        let threads = HashMap::from([("root".to_string(), vec![msg("m1", Role::User, "go")])]);
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["root", "·m1", "+old", ">new"]);
    }

    #[test]
    fn a_turns_tool_calls_are_a_chip_until_the_turn_is_unfolded() {
        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let mut m = msg("m1", Role::Supervisor, "reading it first");
        m.parts.push(Part::ToolCall {
            id: "c1".into(),
            name: "run_steps".into(),
            input: serde_json::json!({"code": "await read(\"util.ts\")"}),
        });
        let threads = HashMap::from([("root".to_string(), vec![m])]);
        let expanded = ids(&["root"]);
        let folded = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert_eq!(shape(&folded), vec!["root", "·m1"]);
        match &folded[1] {
            ForestRow::Message { tools, .. } => assert_eq!(*tools, 1),
            other => panic!("{other:?}"),
        }
        let open = ids(&["m1"]);
        let unfolded = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            tools_open: &open,
            ..Default::default()
        });
        assert_eq!(shape(&unfolded), vec!["root", "·m1", "✦tool:m1:1"]);
        match &unfolded[2] {
            // The PROGRAM, named — not `run_steps`' arguments as JSON.
            ForestRow::Tool { detail, .. } => assert!(detail.contains("util.ts"), "{detail}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_re_rooted_view_starts_at_that_conversations_turns_with_no_row_of_its_own() {
        let root = session_row("root", SessionKind::Root, 1);
        let mut fork = with_origin(session_row("fork", SessionKind::Fork, 2), "root");
        fork.session.origin_message_id = Some("m1".into());
        let sessions = vec![root, fork];
        let threads = HashMap::from([
            ("root".to_string(), vec![msg("m1", Role::User, "go")]),
            ("fork".to_string(), vec![msg("mf", Role::User, "other way")]),
        ]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            root_id: Some("fork"),
            ..Default::default()
        });
        // No `fork` row: the breadcrumb above the list carries the lineage, and
        // the column it would have cost goes to the turns.
        assert_eq!(shape(&rows), vec!["·mf"]);
        assert_eq!(rows[0].depth(), 0);
    }

    #[test]
    fn a_lineage_cycle_renders_a_short_tree_instead_of_hanging_the_terminal() {
        let a = session_row("a", SessionKind::Root, 1);
        let b = with_origin(session_row("b", SessionKind::Fork, 2), "a");
        let sessions = vec![a.clone(), b.clone()];
        let children = HashMap::from([("a".to_string(), vec![b]), ("b".to_string(), vec![a])]);
        let threads = HashMap::from([("a".to_string(), vec![]), ("b".to_string(), vec![])]);
        let expanded = ids(&["a", "b"]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            children_by_origin: &children,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["a", ">b"]);
    }

    #[test]
    fn an_unfetched_thread_is_not_an_empty_one() {
        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let unfetched = forest_rows(&ForestInput {
            sessions: &sessions,
            ..Default::default()
        });
        match &unfetched[0] {
            ForestRow::Session { expandable, .. } => assert!(*expandable),
            other => panic!("{other:?}"),
        }
        let threads = HashMap::from([("root".to_string(), vec![])]);
        let empty = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            ..Default::default()
        });
        match &empty[0] {
            ForestRow::Session { expandable, .. } => assert!(!*expandable),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn branches_sort_by_creation_so_a_row_does_not_move_under_the_cursor() {
        let root = session_row("root", SessionKind::Root, 1);
        let late = with_origin(session_row("late", SessionKind::Fork, 2000), "root");
        let early = with_origin(session_row("early", SessionKind::Fork, 1000), "root");
        let sessions = vec![root, late.clone(), early.clone()];
        let children = HashMap::from([("root".to_string(), vec![late, early])]);
        let threads = HashMap::from([("root".to_string(), vec![])]);
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            children_by_origin: &children,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["root", "+early", ">late"]);
    }

    #[test]
    fn conversations_are_newest_first_this_list_is_also_the_switcher() {
        let old = session_row("old", SessionKind::Root, 1);
        let recent = session_row("recent", SessionKind::Root, 9);
        let sessions = vec![old, recent];
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["recent", "old"]);
    }

    #[test]
    fn the_filter_narrows_the_top_level_and_never_hides_the_open_conversation() {
        let mut a = session_row("a", SessionKind::Root, 1);
        a.session.title = "wire the panel".into();
        let mut b = session_row("b", SessionKind::Root, 2);
        b.session.title = "nightly bench".into();
        let sessions = vec![a, b];
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            filter: Some("bench"),
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["b"]);
        let with_current = forest_rows(&ForestInput {
            sessions: &sessions,
            filter: Some("bench"),
            current_id: Some("a"),
            ..Default::default()
        });
        assert_eq!(shape(&with_current), vec!["b", "a"]);
        let marked = forest_rows(&ForestInput {
            sessions: &sessions,
            current_id: Some("a"),
            ..Default::default()
        });
        assert!(marked
            .iter()
            .any(|r| matches!(r, ForestRow::Session { current, .. } if *current)));
    }

    #[test]
    fn every_turn_is_a_row_and_the_last_one_is_the_active_leaf() {
        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let threads = HashMap::from([("root".to_string(), thread())]);
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["root", "·m1", "·m2", "·m3"]);
        let active: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                ForestRow::Message {
                    id, active: true, ..
                } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(active, vec!["m3"]);
    }

    #[test]
    fn user_only_is_a_filter_on_the_rows_not_on_the_leaf() {
        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let threads = HashMap::from([("root".to_string(), thread())]);
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            user_only: true,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["root", "·m1", "·m3"]);
    }

    #[test]
    fn a_turn_with_no_prose_is_still_a_node() {
        let mut calls = msg("m9", Role::Supervisor, "");
        calls.parts = vec![
            Part::ToolCall {
                id: "c1".into(),
                name: "run_steps".into(),
                input: serde_json::json!({}),
            },
            Part::ToolCall {
                id: "c2".into(),
                name: "run_steps".into(),
                input: serde_json::json!({}),
            },
        ];
        assert_eq!(message_gist(&calls, 56), "(2 steps)");
        let empty = msg("m9", Role::Supervisor, "");
        assert_eq!(message_gist(&empty, 56), "(no text)");
    }

    #[test]
    fn a_long_gist_is_truncated_with_an_ellipsis_a_short_one_is_left_alone() {
        assert_eq!(message_gist(&msg("m1", Role::User, "short"), 56), "short");
        let long = message_gist(&msg("m1", Role::User, &"x".repeat(80)), 56);
        assert_eq!(long.chars().count(), 56);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn enter_follows_pis_selection_rules_addressed_to_the_rows_own_conversation() {
        let f = fixture();
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &f.sessions,
            children_by_origin: &f.children,
            threads: &f.threads,
            expanded: &expanded,
            ..Default::default()
        });
        let at = |id: &str| rows.iter().find(|r| r.id() == id).unwrap().clone();

        assert_eq!(
            selection_for(&at("m1"), &f.threads),
            Selection::Fork {
                session_id: "root".into(),
                at_message_id: "m1".into(),
                exclusive: true,
                editor_text: Some("add a discount function".into()),
            }
        );
        assert_eq!(
            selection_for(&at("m2"), &f.threads),
            Selection::Fork {
                session_id: "root".into(),
                at_message_id: "m2".into(),
                exclusive: false,
                editor_text: None,
            }
        );
        assert_eq!(
            selection_for(&at("root"), &f.threads),
            Selection::Open("root".into())
        );
        assert_eq!(
            selection_for(&at("fork"), &f.threads),
            Selection::Open("fork".into())
        );
        assert_eq!(
            selection_for(&at("collapsed:root"), &f.threads),
            Selection::Drill("root".into())
        );
    }

    #[test]
    fn the_take_back_prefers_a_queued_message_then_the_last_user_turn() {
        let t = thread();
        assert_eq!(
            take_back_target(&["typed while busy".to_string()], &t),
            TakeBack::Queued
        );
        assert_eq!(
            take_back_target(&[], &t),
            TakeBack::Sent {
                at_message_id: "m3".into(),
                text: "now validate pct".into()
            }
        );
        let mut answered = t.clone();
        answered.push(msg("m4", Role::Supervisor, "validating…"));
        assert_eq!(
            take_back_target(&[], &answered),
            TakeBack::Sent {
                at_message_id: "m3".into(),
                text: "now validate pct".into()
            }
        );
        assert_eq!(take_back_target(&[], &[]), TakeBack::None);
        assert_eq!(
            take_back_target(&[], &[msg("m1", Role::Supervisor, "hi")]),
            TakeBack::None
        );
    }

    #[test]
    fn a_taken_back_message_comes_back_verbatim_not_as_a_gist() {
        let multiline = msg("m9", Role::User, "first line\n\n  indented second\n");
        match take_back_target(&[], std::slice::from_ref(&multiline)) {
            TakeBack::Sent { text, .. } => assert_eq!(text, "first line\n\n  indented second\n"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            message_gist(&multiline, usize::MAX),
            "first line indented second"
        );
    }

    #[test]
    fn rewind_lands_on_the_open_conversations_last_user_turn() {
        let f = fixture();
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &f.sessions,
            children_by_origin: &f.children,
            threads: &f.threads,
            expanded: &expanded,
            current_id: Some("root"),
            ..Default::default()
        });
        assert_eq!(rows[rewind_index(&rows, Some("root"))].id(), "m3");

        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let mut t = thread();
        t.push(msg("m4", Role::Supervisor, "validated"));
        let threads = HashMap::from([("root".to_string(), t)]);
        let answered = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            current_id: Some("root"),
            ..Default::default()
        });
        assert_eq!(answered[rewind_index(&answered, Some("root"))].id(), "m3");

        let bare_threads = HashMap::from([("root".to_string(), vec![])]);
        let bare = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &bare_threads,
            ..Default::default()
        });
        assert_eq!(rewind_index(&bare, Some("root")), 0);
        assert_eq!(rewind_index(&bare, None), 0);
    }

    #[test]
    fn running_work_under_a_conversation_is_counted_on_its_row() {
        let root = session_row("root", SessionKind::Root, 1);
        let mut a = with_origin(session_row("a", SessionKind::Subagent, 2), "root");
        a.busy = true;
        let b = with_origin(session_row("b", SessionKind::Subagent, 3), "root");
        let fork = with_origin(session_row("fork", SessionKind::Fork, 4), "root");
        let deep = with_status(
            with_origin(session_row("deep", SessionKind::Subagent, 5), "fork"),
            TurnStatus::Running,
        );
        let sessions = vec![root, fork.clone()];
        let children = HashMap::from([
            ("root".to_string(), vec![a, b, fork]),
            ("fork".to_string(), vec![deep]),
        ]);
        let threads = HashMap::from([("root".to_string(), vec![msg("m1", Role::User, "go")])]);
        let expanded = ids(&["root"]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            children_by_origin: &children,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        let busy_of = |id: &str| {
            rows.iter()
                .find_map(|r| match r {
                    ForestRow::Session {
                        id: rid,
                        busy_below,
                        ..
                    } if rid == id => Some(*busy_below),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(busy_of("root"), 2);
        // The fork is a BRANCH row now — one line, and the live work under it
        // has to survive that flattening or a busy branch reads as an idle one.
        let fork_busy = rows.iter().find_map(|r| match r {
            ForestRow::Branch { id, busy_below, .. } if id == "fork" => Some(*busy_below),
            _ => None,
        });
        // The idle sibling reports nothing, so the count means what it says.
        assert_eq!(fork_busy, Some(1));
    }

    #[test]
    fn reveal_path_names_the_origins_to_expand_to_reach_the_current_conversation() {
        let root = session_row("root", SessionKind::Root, 1);
        let fork = with_origin(session_row("fork", SessionKind::Fork, 2), "root");
        let handoff = with_origin(session_row("hand", SessionKind::Root, 3), "fork");
        let sessions = vec![root.clone(), fork.clone(), handoff];
        let empty: HashMap<String, Vec<SessionRow>> = HashMap::new();

        assert_eq!(
            reveal_path(&sessions, &empty, Some("hand")),
            vec!["root", "fork"]
        );
        assert_eq!(reveal_path(&sessions, &empty, Some("fork")), vec!["root"]);
        assert!(reveal_path(&sessions, &empty, Some("root")).is_empty());
        assert!(reveal_path(&sessions, &empty, None).is_empty());
        assert!(reveal_path(&sessions, &empty, Some("unknown")).is_empty());
        let children = HashMap::from([("root".to_string(), vec![fork])]);
        assert_eq!(reveal_path(&[root], &children, Some("fork")), vec!["root"]);
    }

    #[test]
    fn reveal_path_survives_a_lineage_cycle() {
        let x = with_origin(session_row("x", SessionKind::Fork, 1), "y");
        let y = with_origin(session_row("y", SessionKind::Fork, 2), "x");
        let empty: HashMap<String, Vec<SessionRow>> = HashMap::new();
        let path = reveal_path(&[x, y], &empty, Some("x"));
        assert!(path.len() <= 2, "{path:?}");
    }

    #[test]
    fn a_conversation_whose_messages_match_survives_the_filter() {
        let mut a = session_row("alpha", SessionKind::Root, 1);
        a.session.title = "pricing bug".into();
        let mut b = session_row("beta", SessionKind::Root, 2);
        b.session.title = "unrelated".into();
        let sessions = vec![a, b];
        let session_ids = |rows: Vec<ForestRow>| -> Vec<String> {
            rows.iter()
                .filter_map(|r| match r {
                    ForestRow::Session { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect()
        };
        let base = ForestInput {
            sessions: &sessions,
            filter: Some("compound"),
            ..Default::default()
        };
        // Title-only matching hides both: neither title contains the word.
        assert!(session_ids(forest_rows(&base)).is_empty());
        let matched = vec!["beta".to_string()];
        assert_eq!(
            session_ids(forest_rows(&ForestInput {
                matched_sessions: &matched,
                ..base.clone()
            })),
            vec!["beta"]
        );
        let mut both = session_ids(forest_rows(&ForestInput {
            filter: Some("pricing"),
            matched_sessions: &matched,
            ..base.clone()
        }));
        both.sort();
        assert_eq!(both, vec!["alpha", "beta"]);
        assert_eq!(
            session_ids(forest_rows(&ForestInput {
                current_id: Some("alpha"),
                ..base.clone()
            })),
            vec!["alpha"]
        );
    }

    #[test]
    fn topic_sections_caption_the_turns_beneath_them() {
        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let t = vec![
            msg("m1", Role::User, "fix the discount"),
            msg("m2", Role::Supervisor, "fixed"),
            msg("m3", Role::User, "now the shipping rules"),
            msg("m4", Role::Supervisor, "done"),
        ];
        let threads = HashMap::from([("root".to_string(), t)]);
        let expanded = ids(&["root"]);
        let sections = HashMap::from([(
            "root".to_string(),
            vec![
                SectionRange {
                    start: 0,
                    end: 1,
                    label: "the discount bug".into(),
                },
                SectionRange {
                    start: 2,
                    end: 3,
                    label: "shipping rules".into(),
                },
            ],
        )]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            sections: Some(&sections),
            ..Default::default()
        });
        assert_eq!(
            shape(&rows),
            vec![
                "root",
                "§the discount bug",
                "·m1",
                "·m2",
                "§shipping rules",
                "·m3",
                "·m4"
            ]
        );
        match rows
            .iter()
            .find(|r| matches!(r, ForestRow::Section { .. }))
            .unwrap()
        {
            ForestRow::Section { session_id, .. } => assert_eq!(session_id, "root"),
            other => panic!("{other:?}"),
        }

        // A label with no letters is not a topic.
        let dots = HashMap::from([(
            "root".to_string(),
            vec![
                SectionRange {
                    start: 0,
                    end: 1,
                    label: "…".into(),
                },
                SectionRange {
                    start: 2,
                    end: 3,
                    label: "shipping".into(),
                },
            ],
        )]);
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            sections: Some(&dots),
            ..Default::default()
        });
        assert_eq!(
            shape(&rows),
            vec!["root", "·m1", "·m2", "§shipping", "·m3", "·m4"]
        );

        // Absent sections render no headers — "not fetched" is not "no topics".
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert_eq!(shape(&rows), vec!["root", "·m1", "·m2", "·m3", "·m4"]);
    }

    #[test]
    fn enter_on_a_topic_caption_does_nothing_at_all() {
        let row = ForestRow::Section {
            id: "section:root:0".into(),
            session_id: "root".into(),
            depth: 1,
            label: "the discount bug".into(),
        };
        let threads = HashMap::from([("root".to_string(), vec![msg("m1", Role::User, "fix it")])]);
        assert_eq!(selection_for(&row, &threads), Selection::None);

        let collapsed = ForestRow::Collapsed {
            id: "c".into(),
            origin_id: "root".into(),
            depth: 1,
            count: 2,
        };
        assert_eq!(
            selection_for(&collapsed, &HashMap::new()),
            Selection::Drill("root".into())
        );
    }

    #[test]
    fn a_searched_turn_is_marked_and_only_that_turn() {
        let mut root = session_row("root", SessionKind::Root, 1);
        root.session.title = "pricing".into();
        let sessions = vec![root];
        let t = vec![
            msg("m1", Role::User, "fix the discount"),
            msg("m2", Role::Supervisor, "the compound bug is in fees_total"),
            msg("m3", Role::User, "thanks"),
        ];
        let threads = HashMap::from([("root".to_string(), t)]);
        let expanded = ids(&["root"]);
        let matched_sessions = vec!["root".to_string()];
        let matched_messages = vec!["m2".to_string()];
        let rows = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            filter: Some("compound"),
            matched_sessions: &matched_sessions,
            matched_messages: &matched_messages,
            ..Default::default()
        });
        let marked: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                ForestRow::Message {
                    id, matched: true, ..
                } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(marked, vec!["m2"]);
        let turns: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                ForestRow::Message { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(turns, vec!["m1", "m2", "m3"]);

        let plain = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        assert!(!plain
            .iter()
            .any(|r| matches!(r, ForestRow::Message { matched: true, .. })));
    }
}
