-- git-ai: tell Git AI which lines this agent wrote, and which you did.
--
-- WHAT GIT AI WANTS AND WHERE BOUGH CAN GIVE IT. Git AI attributes lines by
-- CHECKPOINTING twice: once before the agent touches anything, marking
-- everything since the last checkpoint as the human's, and once after, marking
-- what changed as the agent's. Its `agent-v1` preset takes that on stdin, which
-- is the integration path for an agent that is not Claude Code or Cursor
-- (usegitai.com/docs/guides/add-your-agent).
--
-- THE TURN IS THE BOUNDARY, and that is forced rather than chosen. bough is a
-- code-mode harness: the model writes one program per round and that program
-- edits files through host functions inside a sandbox. There is no per-edit
-- hook to hang a checkpoint on. The turn is the tightest boundary that exists.
--
-- THE REPOSITORY IS NOT THE WORKSPACE, and this is the thing that stops the
-- naive version working at all. People run bough from their home directory and
-- let it work across several checkouts — the observed case was a machine where
-- every session's workspace was `~`, which is not a repository, so a hook that
-- asked "is my workspace a repo?" answered no and did nothing, forever. So
-- repositories are discovered from the WORK: the files the turn wrote, and the
-- directories its commands ran in. A turn can touch several, and each is
-- checkpointed on its own.
--
-- WHICH FILES, EXACTLY. `ev.data.edited` on `TurnEnd` is the absolute path of
-- every file this turn's program wrote, recorded by the write verbs
-- themselves. Git cannot answer this — subagents share their spawner's
-- checkout, so a diff at the end is the union of every sibling's work — and
-- neither can a `git status` heuristic when the change was a shell command in
-- some other checkout. The porcelain comparison is still done on top, because
-- a `sed -i` is not a write verb and it is the only thing that sees it.
--
-- A REPOSITORY IS NEVER ATTRIBUTED WITHOUT A BASELINE. Shell commands and
-- bough's file verbs baseline their repository immediately before mutation.
-- A repository discovered only at TurnEnd is still skipped: attributing it
-- would hand Git AI every uncommitted line in it, including the user's.
-- IT IS INERT WITHOUT GIT AI, and finding Git AI is not just `command -v`: the
-- installer puts the binary in `~/.git-ai/bin` and exports that from your shell
-- profile, so a bough started from anything other than an interactive shell
-- does not have it on PATH. Observed on a real machine — installed, hook on,
-- every event silently doing nothing. Both places are checked and the absolute
-- path is what gets invoked.
--
-- NOTHING HERE FAILS A TURN. Every failure is a log line.

local AGENT = "bough"

-- One dispatch has a five second budget and a checkpoint on a big repository is
-- not instant. Four seconds leaves room to return.
local TIMEOUT_MS = 4000

-- The resolved binary, or false for "looked, not there". Asked once.
local bin = nil

-- Per conversation, and deliberately outliving a turn:
--   model      the last model seen on TurnStart
--   messages   the prompts so far — Git AI wants the whole session each time
--   seen       every repository this session has touched, so the next turn
--              baselines it even though this one discovered it too late
--   baselined  repo root -> porcelain snapshot taken at TurnStart
local sessions = {}

-- Repository root per directory. Memoized: this fires on every shell command
-- and a directory does not change which repository it is in.
local roots = {}

local function trim(text)
  return (string.gsub(text or "", "%s+$", ""))
end

local function now()
  return os.date("!%Y-%m-%dT%H:%M:%SZ")
end

local function quote(text)
  return string.format("%q", text)
end

local function git_ai()
  if bin == nil then
    local result = bough.exec(
      'command -v git-ai || { [ -x "$HOME/.git-ai/bin/git-ai" ] && printf %s "$HOME/.git-ai/bin/git-ai"; }'
    )
    local found = nil
    if result ~= nil and result.code == 0 then
      found = trim(result.stdout)
      if found == "" then
        found = nil
      end
    end
    bin = found or false
  end
  return bin or nil
end

-- Where a directory sits in its repository: `{ root, prefix }`, or nil.
--
-- THE ROOT, NEVER THE DIRECTORY ITSELF: `git status --porcelain` reports paths
-- relative to the root wherever it runs from, so treating a subdirectory as the
-- repository names files that do not exist.
--
-- The PREFIX comes back from the same call because it is what lets a file path
-- be written the way git writes it. Git resolves symlinks — on macOS `/var` is
-- `/private/var` — so the path a write verb reported and the path built from a
-- porcelain line are the same file spelled two ways, and sending both hands Git
-- AI one file twice. Rebuilding every path as `root/prefix/name` makes the two
-- sources agree.
local function dir_info(dir)
  if dir == nil or dir == "" then
    return nil
  end
  local cached = roots[dir]
  if cached ~= nil then
    return cached or nil
  end
  local result = bough.exec(
    "git -C " .. quote(dir) .. " rev-parse --show-toplevel --show-prefix"
  )
  local info = nil
  if result ~= nil and result.code == 0 then
    local lines = {}
    for line in string.gmatch(result.stdout, "[^\n]*") do
      table.insert(lines, line)
    end
    local root = trim(lines[1])
    if root ~= "" then
      -- `--show-prefix` is empty at the root and `sub/` below it.
      info = { root = root, prefix = trim(lines[2] or "") }
    end
  end
  -- `false` is a remembered "no repository here"; nil would re-ask every time.
  roots[dir] = info or false
  return info
end

local function toplevel(dir)
  local info = dir_info(dir)
  return info and info.root or nil
end

-- A file as git spells it: the repository root, and its path within it.
local function locate(path)
  local dir, name = string.match(path, "^(.*)/([^/]+)$")
  if dir == nil or dir == "" then
    return nil, nil
  end
  local info = dir_info(dir)
  if info == nil then
    return nil, nil
  end
  return info.root, info.root .. "/" .. info.prefix .. name
end

-- `git status --porcelain` as a map of root-relative path to status.
local function snapshot(root)
  local out = {}
  local result = bough.exec("git -C " .. quote(root) .. " status --porcelain")
  if result == nil or result.code ~= 0 then
    return out
  end
  for line in string.gmatch(result.stdout, "[^\n]+") do
    local status = string.sub(line, 1, 2)
    local path = string.sub(line, 4)
    -- A rename is `R  old -> new`; the new name is the one on disk.
    local renamed = string.match(path, "%->%s+(.+)$")
    out[renamed or path] = status
  end
  return out
end

-- Files whose status moved between two snapshots, as absolute paths.
--
-- A PATH ALREADY DIRTY AT BOTH ENDS IS STILL REPORTED, because `git status`
-- cannot tell "modified before the turn" from "modified again during it" — the
-- code is `` M`` either way. Reporting it is the safe error: Git AI diffs the
-- file itself and attributes only the lines that actually moved, so a file
-- named in vain costs a diff, while a file left out costs the attribution.
local function changed_between(before, after, root, into)
  for path, status in pairs(after) do
    if before[path] ~= status or string.find(status, "M") then
      into[root .. "/" .. path] = true
    end
  end
  -- A file the turn DELETED is gone from `after` but was in `before`; Git AI
  -- still wants to know the line went, and who took it.
  for path, _ in pairs(before) do
    if after[path] == nil then
      into[root .. "/" .. path] = true
    end
  end
end

local function checkpoint(payload)
  local exe = git_ai()
  if exe == nil then
    return
  end
  local result, err = bough.exec(quote(exe) .. " checkpoint agent-v1 --hook-input stdin", {
    stdin = bough.json.encode(payload),
    timeout_ms = TIMEOUT_MS,
  })
  if err ~= nil then
    bough.log.warn("git-ai: " .. err)
  elseif result.code ~= 0 then
    -- stderr, not stdout: git-ai says what it refused and why, and a silent
    -- failure here is an attribution that quietly never happened.
    bough.log.warn("git-ai: checkpoint exited " .. result.code .. ": " .. (result.stderr or ""))
  end
end

local function session_for(id)
  if sessions[id] == nil then
    sessions[id] = { messages = {}, model = "unknown", seen = {}, baselined = {} }
  end
  return sessions[id]
end

-- Mark a repository as one this session works in. Nothing is checkpointed here
-- — a baseline mid-turn would call the agent's earlier edits yours — but the
-- next TurnStart will baseline it.
local function remember(session, root)
  if root ~= nil then
    session.seen[root] = true
  end
end

-- Before the turn: in every repository this session works in, everything on
-- disk that is not already attributed is YOURS.
bough.api.create_autocmd("TurnStart", {
  callback = function(ev)
    local session = session_for(ev.session_id)
    -- Wanted even when there is nothing to baseline: a command later in this
    -- turn may run inside a repository, and it is reported with the model.
    session.model = (ev.data and ev.data.model) or session.model
    if ev.data and ev.data.prompt then
      table.insert(session.messages, {
        type = "user",
        text = ev.data.prompt,
        timestamp = now(),
      })
    end
    session.baselined = {}
    if git_ai() == nil then
      return
    end
    -- The workspace's repository, when it is in one, plus every repository this
    -- session has already been seen working in.
    remember(session, toplevel(ev.workspace))
    for root, _ in pairs(session.seen) do
      session.baselined[root] = snapshot(root)
      checkpoint({ type = "human", repo_working_dir = root })
    end
  end,
})

-- After it: in each of those repositories, what moved is the agent's.
bough.api.create_autocmd("TurnEnd", {
  callback = function(ev)
    local session = session_for(ev.session_id)
    if git_ai() == nil then
      return
    end
    -- The files the turn's own programs wrote, grouped by repository. These
    -- are the ones no diff could attribute to this agent rather than to a
    -- concurrent sibling sharing the checkout.
    local by_repo = {}
    for _, path in ipairs((ev.data and ev.data.edited) or {}) do
      local root, full = locate(path)
      if root ~= nil then
        remember(session, root)
        by_repo[root] = by_repo[root] or {}
        by_repo[root][full] = true
      end
    end
    -- Plus whatever else moved in a repository that was baselined — a `sed -i`
    -- is not a write verb, and the snapshot is the only thing that sees it.
    for root, before in pairs(session.baselined) do
      by_repo[root] = by_repo[root] or {}
      changed_between(before, snapshot(root), root, by_repo[root])
    end
    for root, paths in pairs(by_repo) do
      local edited = {}
      for path, _ in pairs(paths) do
        table.insert(edited, path)
      end
      table.sort(edited)
      if #edited == 0 then
        -- Nothing moved here; a checkpoint over an untouched repository is a
        -- diff for no reason.
      elseif session.baselined[root] == nil then
        -- Discovered too late to baseline. Attributing now would hand Git AI
        -- every uncommitted line in this repository, including yours. The next
        -- turn starts with a baseline and gets it right.
        bough.log.info(
          "git-ai: " .. root .. " was first touched mid-turn; attributing it from the next turn"
        )
      else
        checkpoint({
          type = "ai_agent",
          repo_working_dir = root,
          agent_name = AGENT,
          model = session.model,
          conversation_id = ev.session_id,
          edited_filepaths = edited,
          -- The prompts only. bough's hooks see what you asked for, never what
          -- the model answered, and inventing an assistant turn to fill the
          -- shape would be a transcript that says something nobody said.
          transcript = { messages = session.messages },
        })
      end
    end
    session.baselined = {}
  end,
})

-- Every bough mutation is baselined before it changes a repository. Shell
-- commands identify their checkout from the command directory; write() and
-- patch() report their target with PreWrite. That preserves a first turn in a
-- repository outside the session workspace instead of dropping its transcript.
local function baseline(session, root)
  remember(session, root)
  if session.baselined[root] == nil then
    session.baselined[root] = snapshot(root)
    checkpoint({ type = "human", repo_working_dir = root })
  end
end

local function baseline_before_shell_command(ev, command)
  if command == nil or command == "" or git_ai() == nil then
    return
  end
  local root = toplevel(ev.workspace)
  if root == nil then
    return
  end
  baseline(session_for(ev.session_id), root)
end

local function baseline_before_file_write(ev)
  local path = ev.data and ev.data.path
  if path == nil or path == "" or git_ai() == nil then
    return
  end
  local root = locate(path)
  if root == nil then
    return
  end
  baseline(session_for(ev.session_id), root)
end

bough.api.create_autocmd("PreTool", {
  callback = function(ev)
    baseline_before_shell_command(ev, ev.data and ev.data.input and ev.data.input.command)
  end,
})

bough.api.create_autocmd("PreWrite", {
  callback = function(ev)
    baseline_before_file_write(ev)
  end,
})
