-- git-ai: tell Git AI which lines this agent wrote, and which you did.
--
-- WHAT GIT AI WANTS AND WHERE BOUGH CAN GIVE IT. Git AI attributes lines by
-- CHECKPOINTING twice: once before the agent touches anything, marking
-- everything since the last checkpoint as the human's, and once after, marking
-- what changed as the agent's. Its `agent-v1` preset takes that on stdin, which
-- is the integration path for an agent that is not Claude Code or Cursor
-- (usegitai.com/docs/guides/add-your-agent).
--
-- THE BOUNDARY IS THE TURN, NOT THE EDIT, and that is forced rather than
-- chosen. bough is a code-mode harness: the model writes one program per round
-- and that program edits files through host functions inside a sandbox. There
-- is no per-edit hook to hang a checkpoint on — `PreTool` and `PostTool` fire
-- for shell commands only. The turn boundary is the tightest one that exists,
-- and it is the right shape anyway: everything on disk before `TurnStart` is
-- yours, everything that moved by `TurnEnd` is the agent's.
--
-- EDITED PATHS COME FROM A BEFORE/AFTER COMPARISON, not from watching the
-- agent. `git status --porcelain` at both ends, and what differs between them
-- is what this turn touched. Passing them is what keeps `checkpoint` fast on a
-- large repo (Git AI narrows its diff to the files it is told about), and it is
-- the difference between attributing a turn and attributing a repository.
--
-- IT IS INERT WITHOUT GIT AI. No binary on PATH, or a workspace that is not a
-- git repository, and every callback returns immediately. That check is made
-- once per process, not once per turn: `command -v` on every event of every
-- turn is a subprocess for nothing.
--
-- NOTHING HERE FAILS A TURN. Every failure is a log line — Git AI not
-- recording a checkpoint is worse than nothing recorded, and both are far
-- better than a turn that did not run because an attribution tool was unhappy.

local AGENT = "bough"

-- Git AI's own advice: check once, at startup, and never assume the binary is
-- there. `nil` is "not yet asked".
local installed = nil

-- Per conversation: the model, the porcelain snapshot taken at TurnStart, and
-- the transcript so far. Git AI wants the WHOLE session on every call, not the
-- new messages, so it accumulates.
local sessions = {}

-- One dispatch has a five second budget and a checkpoint on a big repo is not
-- instant. Four seconds leaves room to return.
local TIMEOUT_MS = 4000

local function now()
  return os.date("!%Y-%m-%dT%H:%M:%SZ")
end

local function have_git_ai()
  if installed == nil then
    local result = bough.exec("command -v git-ai")
    installed = result ~= nil and result.code == 0
  end
  return installed
end

-- The REPOSITORY a directory is in, or nil.
--
-- THE ROOT, NEVER THE DIRECTORY ITSELF. bough is pointed at a workspace, and a
-- workspace is very often a subdirectory of the repository — a crate inside a
-- monorepo, a package inside a checkout. `git status --porcelain` reports paths
-- relative to the ROOT wherever it is run from, so treating the workspace as
-- the repository named files that do not exist (`/repo/sub/sub/f.txt`) and
-- asked Git AI to attribute nothing.
--
-- Memoized per directory, because this fires on every shell command and a
-- directory does not change which repository it is in.
local roots = {}

local function toplevel(dir)
  if dir == nil or dir == "" then
    return nil
  end
  local cached = roots[dir]
  if cached ~= nil then
    return cached or nil
  end
  local result = bough.exec("git -C " .. string.format("%q", dir) .. " rev-parse --show-toplevel")
  local root = nil
  if result ~= nil and result.code == 0 then
    root = string.gsub(result.stdout, "%s+$", "")
    if root == "" then
      root = nil
    end
  end
  -- `false` is a remembered "no repository here"; nil would re-ask every time.
  roots[dir] = root or false
  return root
end

-- `git status --porcelain` as a map of path to status. The status is part of
-- the value, not just the key: a file already modified before the turn and
-- modified again during it keeps the same path and the same `` M`` code, so
-- the map is only half the answer — see `edited_since`.
local function snapshot(root)
  local out = {}
  local result = bough.exec("git -C " .. string.format("%q", root) .. " status --porcelain")
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

-- Files whose status changed between two snapshots.
--
-- A PATH ALREADY DIRTY AT BOTH ENDS IS STILL REPORTED, because `git status`
-- cannot tell "modified before the turn" from "modified again during it" — the
-- code is `` M`` either way. Reporting it is the safe error: Git AI diffs the
-- file itself and attributes only the lines that actually moved, so a file
-- named in vain costs a diff, while a file left out costs the attribution.
local function edited_since(before, after, root)
  local paths = {}
  for path, status in pairs(after) do
    if before[path] ~= status or string.sub(status, 1, 1) == "M" or string.sub(status, 2, 2) == "M" then
      table.insert(paths, root .. "/" .. path)
    end
  end
  -- A file the turn DELETED is gone from `after` but was in `before`; Git AI
  -- still wants to know the line went, and who took it.
  for path, _ in pairs(before) do
    if after[path] == nil then
      table.insert(paths, root .. "/" .. path)
    end
  end
  table.sort(paths)
  return paths
end

local function checkpoint(payload)
  -- `git-ai`, not `git ai`. They are the same binary, but the `git ai` form
  -- goes through the shim Git AI's installer puts on PATH ahead of git, and
  -- this hook's own check is `command -v git-ai` — testing for one thing and
  -- then invoking another is how an install with the binary but without the
  -- shim turns into an error on every event of every turn.
  local result, err = bough.exec("git-ai checkpoint agent-v1 --hook-input stdin", {
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
    sessions[id] = { messages = {}, model = "unknown", before = {}, root = nil }
  end
  return sessions[id]
end

-- The repository this event is about, or nil when there is nothing to record:
-- no Git AI installed, or a directory that is not in a repository at all.
--
-- IT IS THE EVENT'S DIRECTORY, NOT THE SESSION'S. A shell event carries the
-- directory the command runs in, which is how a command that works inside a
-- repository is attributed to THAT repository even when bough itself was
-- started somewhere else entirely.
local function repo_of(ev)
  if not have_git_ai() then
    return nil
  end
  return toplevel(ev.workspace)
end

-- Before the turn: everything on disk that is not already attributed is YOURS.
bough.api.create_autocmd("TurnStart", {
  callback = function(ev)
    local root = repo_of(ev)
    local session = session_for(ev.session_id)
    -- The model rides TurnStart and is wanted even when there is no repository
    -- to checkpoint: a shell command later in this turn may be inside one.
    session.model = (ev.data and ev.data.model) or session.model
    if root == nil then
      -- NOTHING IS BASELINED LATER. If this turn goes on to touch a repository
      -- through a shell command, that command is attributed by its own pre/post
      -- pair. Taking a `human` baseline at that point instead would mark
      -- everything the agent had already written as yours, which is the one
      -- error worth refusing to make.
      session.root = nil
      return
    end
    session.root = root
    session.before = snapshot(root)
    if ev.data and ev.data.prompt then
      table.insert(session.messages, {
        type = "user",
        text = ev.data.prompt,
        timestamp = now(),
      })
    end
    checkpoint({
      type = "human",
      repo_working_dir = root,
    })
  end,
})

-- After it: what moved is the agent's, with the session that produced it.
bough.api.create_autocmd("TurnEnd", {
  callback = function(ev)
    local session = session_for(ev.session_id)
    -- The repository BASELINED AT TURNSTART, not whatever this event points at:
    -- the pair has to be about one repository or the diff is meaningless.
    local root = session.root
    if root == nil or not have_git_ai() then
      return
    end
    local edited = edited_since(session.before, snapshot(root), root)
    -- A turn that edited nothing has nothing to attribute, and a checkpoint
    -- over an untouched repository is a diff for no reason.
    if #edited == 0 then
      return
    end
    checkpoint({
      type = "ai_agent",
      repo_working_dir = root,
      agent_name = AGENT,
      model = session.model,
      conversation_id = ev.session_id,
      edited_filepaths = edited,
      -- The prompts only. bough's hooks see what you asked for, never what the
      -- model answered, and inventing an assistant turn to fill the shape
      -- would be a transcript that says something nobody said.
      transcript = { messages = session.messages },
    })
    session.before = {}
  end,
})

-- A shell command the agent ran can change files too — a `sed -i`, a
-- formatter, a code generator. Git AI's agent-v1 preset takes the pair
-- directly and folds it into the same attribution.
local function shell_event(kind, ev, command)
  if command == nil or command == "" then
    return
  end
  local root = repo_of(ev)
  if root == nil then
    return
  end
  local session = session_for(ev.session_id)
  checkpoint({
    type = kind,
    repo_working_dir = root,
    agent_name = AGENT,
    model = session.model,
    conversation_id = ev.session_id,
    command = command,
  })
end

bough.api.create_autocmd("PreTool", {
  callback = function(ev)
    shell_event("pre_shell_command", ev, ev.data and ev.data.input and ev.data.input.command)
  end,
})

bough.api.create_autocmd("PostTool", {
  callback = function(ev)
    shell_event("post_shell_command", ev, ev.data and ev.data.command)
  end,
})
