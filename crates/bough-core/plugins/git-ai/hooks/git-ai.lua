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

local function is_repo(workspace)
  if workspace == nil or workspace == "" then
    return false
  end
  local result = bough.exec("git -C " .. string.format("%q", workspace) .. " rev-parse --git-dir")
  return result ~= nil and result.code == 0
end

-- `git status --porcelain` as a map of path to status. The status is part of
-- the value, not just the key: a file already modified before the turn and
-- modified again during it keeps the same path and the same `` M`` code, so
-- the map is only half the answer — see `edited_since`.
local function snapshot(workspace)
  local out = {}
  local result = bough.exec("git -C " .. string.format("%q", workspace) .. " status --porcelain")
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
local function edited_since(before, after, workspace)
  local paths = {}
  for path, status in pairs(after) do
    if before[path] ~= status or string.sub(status, 1, 1) == "M" or string.sub(status, 2, 2) == "M" then
      table.insert(paths, workspace .. "/" .. path)
    end
  end
  -- A file the turn DELETED is gone from `after` but was in `before`; Git AI
  -- still wants to know the line went, and who took it.
  for path, _ in pairs(before) do
    if after[path] == nil then
      table.insert(paths, workspace .. "/" .. path)
    end
  end
  table.sort(paths)
  return paths
end

local function checkpoint(payload)
  local result, err = bough.exec("git ai checkpoint agent-v1 --hook-input stdin", {
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
    sessions[id] = { messages = {}, model = "unknown", before = {} }
  end
  return sessions[id]
end

local function ready(ev)
  return have_git_ai() and is_repo(ev.workspace)
end

-- Before the turn: everything on disk that is not already attributed is YOURS.
bough.api.create_autocmd("TurnStart", {
  callback = function(ev)
    if not ready(ev) then
      return
    end
    local session = session_for(ev.session_id)
    session.model = (ev.data and ev.data.model) or session.model
    session.before = snapshot(ev.workspace)
    if ev.data and ev.data.prompt then
      table.insert(session.messages, {
        type = "user",
        text = ev.data.prompt,
        timestamp = now(),
      })
    end
    checkpoint({
      type = "human",
      repo_working_dir = ev.workspace,
    })
  end,
})

-- After it: what moved is the agent's, with the session that produced it.
bough.api.create_autocmd("TurnEnd", {
  callback = function(ev)
    if not ready(ev) then
      return
    end
    local session = session_for(ev.session_id)
    local edited = edited_since(session.before, snapshot(ev.workspace), ev.workspace)
    -- A turn that edited nothing has nothing to attribute, and a checkpoint
    -- over an untouched repository is a diff for no reason.
    if #edited == 0 then
      return
    end
    checkpoint({
      type = "ai_agent",
      repo_working_dir = ev.workspace,
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
  if not ready(ev) or command == nil or command == "" then
    return
  end
  local session = session_for(ev.session_id)
  checkpoint({
    type = kind,
    repo_working_dir = ev.workspace,
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
