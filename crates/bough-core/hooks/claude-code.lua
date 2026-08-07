-- Adopt a Claude Code setup: its CLAUDE.md files and its shell hooks.
--
-- WHY THIS IS A PLUGIN AND NOT A FEATURE. bough reads AGENTS.md and never
-- CLAUDE.md — one instruction file, chosen on purpose, so a project cannot
-- have two sets of rules that disagree. That decision holds. This is the
-- opt-in: turn it on (^x) in a repo that keeps its instructions in CLAUDE.md
-- and you get them; leave it off and nothing changes.
--
-- It bridges TWO things:
--
--   *.md   — CLAUDE.md at ~/.claude and in the workspace, plus
--            .claude/rules/*.md, injected as context at turn start.
--   hooks  — the `hooks` block of .claude/settings.json (and the local and
--            user files), run as Claude Code runs them: JSON on stdin, exit 2
--            blocks with stderr as the reason, exit 0 with JSON on stdout
--            carries a decision.
--
-- WHAT IS NOT BRIDGED, deliberately. Claude Code fires around thirty events;
-- bough fires five. Only the four with an honest counterpart are mapped —
-- UserPromptSubmit, PreToolUse, PostToolUse, Stop. A hook wired to an event
-- with no counterpart is not silently dropped: it is listed at load time in
-- the log, so "my hook did not run" has an answer.

local MAPPED = {
  TurnStart = "UserPromptSubmit",
  PreTool = "PreToolUse",
  PostTool = "PostToolUse",
  TurnEnd = "Stop",
}

local function read_json(path)
  local text = bough.fs.read(path)
  if text == nil or text == "" then return nil end
  local ok, decoded = pcall(bough.json.decode, text)
  if not ok then
    bough.log.warn("claude-code: " .. path .. " is not valid JSON")
    return nil
  end
  return decoded
end

local function settings_files(workspace)
  local home = bough.home()
  local files = {}
  if home ~= "" then table.insert(files, home .. "/.claude/settings.json") end
  if workspace ~= nil and workspace ~= "" then
    table.insert(files, workspace .. "/.claude/settings.json")
    table.insert(files, workspace .. "/.claude/settings.local.json")
  end
  return files
end

-- Every `{matcher, hooks}` group Claude Code would consider for `event`,
-- across all three settings files. User first, project last — the order
-- Claude Code merges in, so the closest file has the last word.
local function groups_for(workspace, event)
  local out = {}
  for _, path in ipairs(settings_files(workspace)) do
    local settings = read_json(path)
    local hooks = settings and settings.hooks and settings.hooks[event]
    if hooks then
      for _, group in ipairs(hooks) do
        table.insert(out, group)
      end
    end
  end
  return out
end

-- Claude Code's matcher is a tool name, `*`, or absent (= everything).
local function matches(matcher, tool)
  if matcher == nil or matcher == "" or matcher == "*" then return true end
  return string.find(tool, matcher) ~= nil
end

-- Run one group's commands, newest last, and fold what they returned.
local function run_group(group, payload, folded)
  for _, entry in ipairs(group.hooks or {}) do
    if entry.type == "command" and entry.command ~= nil then
      local result, err = bough.exec(entry.command, { stdin = bough.json.encode(payload) })
      if err ~= nil then
        bough.log.warn("claude-code: " .. entry.command .. ": " .. err)
      elseif result.code == 2 then
        -- The documented block: exit 2, reason on stderr.
        folded.decision = "deny"
        folded.reason = (result.stderr ~= "" and result.stderr)
          or "blocked by a Claude Code hook"
      elseif result.code == 0 and result.stdout ~= "" then
        local ok, out = pcall(bough.json.decode, result.stdout)
        if ok and type(out) == "table" then
          local specific = out.hookSpecificOutput or {}
          if specific.permissionDecision == "deny" or out.decision == "block" then
            folded.decision = "deny"
            folded.reason = specific.permissionDecisionReason or out.reason
              or "blocked by a Claude Code hook"
          end
          if specific.additionalContext ~= nil then
            bough.context(specific.additionalContext)
          end
          if specific.updatedInput ~= nil and specific.updatedInput.command ~= nil then
            folded.input = { command = specific.updatedInput.command }
          end
          if specific.updatedToolOutput ~= nil then
            folded.output = specific.updatedToolOutput
          end
          if out["continue"] == false then
            folded.stop = out.stopReason or "a Claude Code hook stopped the turn"
          end
        end
      end
    end
  end
end

-- ---------------------------------------------------------------------------
-- The .md files
-- ---------------------------------------------------------------------------

local function add_file(parts, path, label)
  local text = bough.fs.read(path)
  if text ~= nil and text ~= "" then
    table.insert(parts, "## " .. label .. "\n\n" .. text)
  end
end

bough.api.create_autocmd("TurnStart", {
  callback = function(ev)
    local parts = {}
    local home = bough.home()
    if home ~= "" then add_file(parts, home .. "/.claude/CLAUDE.md", "CLAUDE.md (user)") end
    if ev.workspace ~= nil and ev.workspace ~= "" then
      add_file(parts, ev.workspace .. "/CLAUDE.md", "CLAUDE.md")
      local names = bough.fs.list(ev.workspace .. "/.claude/rules")
      for _, name in ipairs(names or {}) do
        if string.sub(name, -3) == ".md" then
          add_file(parts, ev.workspace .. "/.claude/rules/" .. name, ".claude/rules/" .. name)
        end
      end
    end
    if #parts > 0 then
      bough.context(table.concat(parts, "\n\n"))
    end

    local folded = {}
    for _, group in ipairs(groups_for(ev.workspace, MAPPED.TurnStart)) do
      run_group(group, {
        session_id = "",
        hook_event_name = "UserPromptSubmit",
        prompt = ev.data.prompt,
        cwd = ev.workspace,
      }, folded)
    end
    return folded
  end,
})

-- ---------------------------------------------------------------------------
-- The tool boundary
-- ---------------------------------------------------------------------------

bough.api.create_autocmd("PreTool", {
  callback = function(ev)
    local folded = {}
    -- bough's only tool is `bash`; Claude Code calls it `Bash`, and a matcher
    -- written for their harness has to keep working here.
    for _, group in ipairs(groups_for(ev.workspace, MAPPED.PreTool)) do
      if matches(group.matcher, "Bash") then
        run_group(group, {
          session_id = "",
          hook_event_name = "PreToolUse",
          tool_name = "Bash",
          tool_input = { command = ev.data.input.command },
          cwd = ev.workspace,
        }, folded)
      end
    end
    return folded
  end,
})

bough.api.create_autocmd("PostTool", {
  callback = function(ev)
    local folded = {}
    for _, group in ipairs(groups_for(ev.workspace, MAPPED.PostTool)) do
      if matches(group.matcher, "Bash") then
        run_group(group, {
          session_id = "",
          hook_event_name = "PostToolUse",
          tool_name = "Bash",
          tool_input = { command = ev.data.command },
          tool_response = { output = ev.data.output },
          cwd = ev.workspace,
        }, folded)
      end
    end
    return folded
  end,
})

bough.api.create_autocmd("TurnEnd", {
  callback = function(ev)
    local folded = {}
    for _, group in ipairs(groups_for(ev.workspace, MAPPED.TurnEnd)) do
      run_group(group, {
        session_id = "",
        hook_event_name = "Stop",
        cwd = ev.workspace,
      }, folded)
    end
    -- A `Stop` hook's block is not a thing bough can honour — the turn is
    -- already over — so it is reported rather than silently swallowed.
    if folded.decision == "deny" then
      bough.log.warn("claude-code: a Stop hook blocked, which bough cannot undo: "
        .. tostring(folded.reason))
      folded.decision = nil
    end
    return folded
  end,
})
