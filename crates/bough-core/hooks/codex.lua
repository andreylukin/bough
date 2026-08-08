-- Adopt a Codex CLI setup: its global AGENTS.md, its hooks, and its `notify`.
--
-- WHAT CODEX ACTUALLY HAS. Its instruction chain is `~/.codex/AGENTS.override.md`
-- or `~/.codex/AGENTS.md` (first non-empty wins), then one file per directory
-- from the repository root down to the cwd, checking `AGENTS.override.md` then
-- `AGENTS.md`, concatenated in that order so the nearest file has the last
-- word.
--
-- ITS HOOKS ARE A FULL FRAMEWORK, not just `notify`. This file used to say
-- notify was "its only hook" and read nothing else, which was true once and is
-- not now: Codex discovers `hooks.json` beside each active config layer, with
-- the SAME three-level shape as Claude Code (event -> matcher groups ->
-- handlers) and the same decision contract — exit 2 with stderr, or JSON with
-- `hookSpecificOutput.permissionDecision` / `decision: "block"` /
-- `additionalContext` / `updatedInput`. So the adapter below is deliberately
-- the same shape as `claude-code.lua`'s.
--
-- WHAT IS NOT ADOPTED, and why each one is a decision rather than an omission:
--
--   inline `[hooks]` in config.toml — Codex accepts hooks as TOML tables as
--     well as JSON. Hand-parsing nested arrays-of-tables out of TOML, in a
--     file bough otherwise has no reason to understand, is the kind of parse
--     whose failure mode is RUNNING THE WRONG COMMAND. It is warned about at
--     load instead, so "my hook did not run" has an answer.
--   plugin-bundled hooks — Codex has no installed-plugins registry; a
--     marketplace lists what is AVAILABLE, and Codex itself will not run a
--     plugin's hooks until you review and trust each one. Adopting every
--     marketplace entry's commands would run code nobody chose, which is the
--     one thing `hooks::sources` refuses to do. Their skills are still read
--     (`skills/foreign.rs`); only their commands are withheld.
--   events with no counterpart — bough fires five. `SessionStart`,
--     `PermissionRequest`, `PreCompact` and the rest have no honest mapping,
--     and a mapping that is not honest is worse than none.
--
-- bough reads the workspace `AGENTS.md` natively; re-injecting it here would
-- put the same text in the prompt twice, which costs tokens and teaches the
-- model that repetition means emphasis.
--
-- `notify` is fire-and-forget by Codex's own contract: it cannot decide
-- anything, and neither does this.

local MAPPED = {
  TurnStart = "UserPromptSubmit",
  PreTool = "PreToolUse",
  PostTool = "PostToolUse",
  TurnEnd = "Stop",
}

local function first_readable(paths)
  for _, path in ipairs(paths) do
    local text = bough.fs.read(path)
    if text ~= nil and text ~= "" then
      return text, path
    end
  end
  return nil, nil
end

local function read_json(path)
  local text = bough.fs.read(path)
  if text == nil or text == "" then return nil end
  local ok, decoded = pcall(bough.json.decode, text)
  if not ok then
    bough.log.warn("codex: " .. path .. " is not valid JSON")
    return nil
  end
  return decoded
end

-- The config layers Codex would consider, lowest precedence first. Every
-- matching hook from every layer runs — Codex does not let a nearer layer
-- replace a farther one — so this is a concatenation, not a lookup.
local function hook_files(workspace)
  local files = {}
  local home = bough.home()
  if home ~= "" then table.insert(files, home .. "/.codex/hooks.json") end
  if workspace ~= nil and workspace ~= "" then
    table.insert(files, workspace .. "/.codex/hooks.json")
  end
  return files
end

-- Every `{matcher, hooks}` group configured for `event`.
local function groups_for(workspace, event)
  local out = {}
  for _, path in ipairs(hook_files(workspace)) do
    local doc = read_json(path)
    local hooks = doc and doc.hooks and doc.hooks[event]
    for _, group in ipairs(hooks or {}) do
      table.insert(out, group)
    end
  end
  return out
end

-- Codex's matcher is a regex over the tool name; `*`, `""` and absent all mean
-- everything. bough has one tool, so this only ever answers "does this fire on
-- Bash" — `Bash` and `^Bash$` do, `Edit|Write` does not. (Lua patterns have no
-- alternation, so a matcher written with `|` never matches here, which for the
-- single-tool case gives the right answer for the wrong reason.)
local function matches(matcher, tool)
  if matcher == nil or matcher == "" or matcher == "*" then return true end
  local ok, found = pcall(string.find, tool, matcher)
  return ok and found ~= nil
end

-- Run one group's handlers and fold what they returned. The contract is
-- Codex's, which is Claude Code's: exit 2 blocks with stderr as the reason,
-- exit 0 with JSON on stdout carries a decision.
local function run_group(group, payload, folded)
  for _, entry in ipairs(group.hooks or {}) do
    if entry.type == "command" and entry.command ~= nil then
      local opts = { stdin = bough.json.encode(payload) }
      -- Codex's timeout is in seconds; bough's exec takes milliseconds.
      if type(entry.timeout) == "number" then
        opts.timeout_ms = math.floor(entry.timeout * 1000)
      end
      local result, err = bough.exec(entry.command, opts)
      if err ~= nil then
        bough.log.warn("codex: " .. entry.command .. ": " .. err)
      elseif result.code == 2 then
        folded.decision = "deny"
        folded.reason = (result.stderr ~= "" and result.stderr)
          or "blocked by a Codex hook"
      elseif result.code == 0 and result.stdout ~= "" then
        local ok, out = pcall(bough.json.decode, result.stdout)
        if ok and type(out) == "table" then
          local specific = out.hookSpecificOutput or {}
          if specific.permissionDecision == "deny" or out.decision == "block" then
            folded.decision = "deny"
            folded.reason = specific.permissionDecisionReason or out.reason
              or "blocked by a Codex hook"
          end
          if specific.additionalContext ~= nil then
            bough.context(specific.additionalContext)
          end
          if specific.permissionDecision == "allow"
            and specific.updatedInput ~= nil
            and specific.updatedInput.command ~= nil then
            folded.input = { command = specific.updatedInput.command }
          end
          if out["continue"] == false then
            folded.stop = out.stopReason or "a Codex hook stopped the turn"
          end
        end
      end
    end
  end
end

-- The `notify = ["program", "arg"]` array out of config.toml.
--
-- Parsed by hand rather than with a TOML reader: this is one array of strings
-- at the top level of a file bough otherwise has no reason to understand, and
-- a wrong parse here must degrade to "no notify", never to running the wrong
-- program.
local function notify_argv(text)
  if text == nil then return nil end
  local line = string.match(text, "\n%s*notify%s*=%s*(%b[])")
    or string.match(text, "^%s*notify%s*=%s*(%b[])")
  if line == nil then return nil end
  local argv = {}
  for item in string.gmatch(line, '"([^"]*)"') do
    table.insert(argv, item)
  end
  if #argv == 0 then return nil end
  return argv
end

local function shell_quote(text)
  return "'" .. string.gsub(text, "'", "'\\''") .. "'"
end

bough.api.create_autocmd("TurnStart", {
  callback = function(ev)
    local home = bough.home()
    local parts = {}

    if home ~= "" then
      local text, path = first_readable({
        home .. "/.codex/AGENTS.override.md",
        home .. "/.codex/AGENTS.md",
      })
      if text ~= nil then
        table.insert(parts, "## " .. path .. "\n\n" .. text)
      end
    end

    -- The per-directory chain, WHICH RUNS UPWARD. Codex reads one file per
    -- directory from the repository root DOWN TO the cwd — those directories
    -- are the workspace's ANCESTORS. This used to list the workspace's
    -- children instead, which is not Codex's chain at all: in a monorepo it
    -- put every sibling package's rules into every turn.
    --
    -- Only `AGENTS.override.md` is collected. The `AGENTS.md` half of each
    -- rung is already read natively by `prompt/project.rs`, over the same
    -- git-root-down chain, and saying it twice is not saying it louder. The
    -- override file is the part that reader does not know about.
    if ev.workspace ~= nil and ev.workspace ~= "" then
      local chain = {}
      local dir = ev.workspace
      local found_root = false
      for _ = 1, 24 do
        table.insert(chain, 1, dir)
        for _, name in ipairs(bough.fs.list(dir) or {}) do
          if name == ".git" then found_root = true end
        end
        local parent = string.match(dir, "^(.*)/[^/]+$")
        if found_root or parent == nil or parent == "" then break end
        dir = parent
      end
      -- No git root above: read only the workspace, rather than adopting
      -- whatever sits in the user's home directory. Same rule, same reason,
      -- as `find_project_rules`.
      if not found_root then chain = { ev.workspace } end
      for _, d in ipairs(chain) do
        local text = bough.fs.read(d .. "/AGENTS.override.md")
        if text ~= nil and text ~= "" then
          table.insert(parts, "## " .. d .. "/AGENTS.override.md\n\n" .. text)
        end
      end
    end

    if #parts > 0 then
      bough.context(table.concat(parts, "\n\n"))
    end

    -- An inline `[hooks]` table is configuration bough can see but will not
    -- parse. Said once per turn rather than never, because a hook that is
    -- configured, enabled in Codex, and silently absent here is the exact
    -- support question this adapter exists to prevent.
    local home_toml = home ~= "" and bough.fs.read(home .. "/.codex/config.toml")
    if type(home_toml) == "string" and string.find(home_toml, "%[%[?hooks[%.%]]") then
      bough.log.warn("codex: inline [hooks] in config.toml is not adopted — "
        .. "move them to ~/.codex/hooks.json and bough will run them")
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
    -- bough's only tool is `bash`; Codex's canonical hook name for it is
    -- `Bash`, and a matcher written for that harness has to keep working here.
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

bough.api.create_autocmd({ "TurnEnd", "TurnError" }, {
  callback = function(ev)
    -- `Stop` fires when the model finishes, so a failed turn is not one. Its
    -- block is not a thing bough can honour — the turn is already over — so it
    -- is reported rather than silently swallowed, as in `claude-code.lua`.
    if ev.event ~= "TurnError" then
      local folded = {}
      for _, group in ipairs(groups_for(ev.workspace, MAPPED.TurnEnd)) do
        run_group(group, {
          session_id = "",
          hook_event_name = "Stop",
          cwd = ev.workspace,
        }, folded)
      end
      if folded.decision == "deny" then
        bough.log.warn("codex: a Stop hook blocked, which bough cannot undo: "
          .. tostring(folded.reason))
      end
    end

    local home = bough.home()
    if home == "" then return end
    local argv = notify_argv(bough.fs.read(home .. "/.codex/config.toml"))
    if argv == nil then return end

    -- Codex hands its notify program one argument: a JSON object. The shape
    -- is theirs, so a script written for Codex keeps working.
    local payload = bough.json.encode({
      type = "agent-turn-complete",
      ["turn-id"] = "",
      ["last-assistant-message"] = ev.event == "TurnError" and "turn failed" or "",
    })
    local command = {}
    for _, part in ipairs(argv) do
      table.insert(command, shell_quote(part))
    end
    table.insert(command, shell_quote(payload))
    local _, err = bough.exec(table.concat(command, " "), { timeout_ms = 2000 })
    if err ~= nil then
      bough.log.warn("codex: notify failed: " .. err)
    end
  end,
})
