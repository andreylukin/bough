-- Adopt a Codex CLI setup: its global AGENTS.md and its `notify` program.
--
-- WHAT CODEX ACTUALLY HAS. Its instruction chain is `~/.codex/AGENTS.override.md`
-- or `~/.codex/AGENTS.md` (first non-empty wins), then one file per directory
-- from the repository root down to the cwd, checking `AGENTS.override.md` then
-- `AGENTS.md`, concatenated in that order so the nearest file has the last
-- word. Its only hook is `notify` in `~/.codex/config.toml` — a program run
-- with a JSON argument when a turn ends.
--
-- SO THIS PLUGIN IS SMALL, and most of it is the part bough does not already
-- do. bough reads the workspace `AGENTS.md` natively; re-injecting it here
-- would put the same text in the prompt twice, which costs tokens and teaches
-- the model that repetition means emphasis. What is missing is the GLOBAL
-- file — the rules a Codex user keeps in `~/.codex` and would otherwise lose
-- — and the per-directory chain below the workspace root.
--
-- `notify` is fire-and-forget by Codex's own contract: it cannot decide
-- anything, and neither does this.

local function first_readable(paths)
  for _, path in ipairs(paths) do
    local text = bough.fs.read(path)
    if text ~= nil and text ~= "" then
      return text, path
    end
  end
  return nil, nil
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

    -- The per-directory chain UNDER the workspace root. The root's own file is
    -- skipped: bough already read it, and saying it twice is not saying it
    -- louder.
    if ev.workspace ~= nil and ev.workspace ~= "" then
      local names = bough.fs.list(ev.workspace) or {}
      for _, name in ipairs(names) do
        local sub = ev.workspace .. "/" .. name
        local text, path = first_readable({
          sub .. "/AGENTS.override.md",
          sub .. "/AGENTS.md",
        })
        if text ~= nil then
          table.insert(parts, "## " .. path .. "\n\n" .. text)
        end
      end
    end

    if #parts > 0 then
      bough.context(table.concat(parts, "\n\n"))
    end
  end,
})

bough.api.create_autocmd({ "TurnEnd", "TurnError" }, {
  callback = function(ev)
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
