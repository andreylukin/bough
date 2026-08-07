-- Refuse the shell commands that cannot be undone.
--
-- Off by default, like every bundled hook: turn it on in the hooks panel
-- (^x) if you want it. It denies rather than asks, because a hook has no
-- way to prompt you — the model reads the reason and picks another route.

local DESTRUCTIVE = {
  { pattern = "rm%s+%-[%w]*[rf]", why = "recursive or forced rm" },
  { pattern = "git%s+push%s+.*%-%-force", why = "a force push" },
  { pattern = "git%s+reset%s+%-%-hard", why = "a hard reset" },
  { pattern = "git%s+clean%s+%-[%w]*f", why = "git clean -f" },
  { pattern = "DROP%s+TABLE", why = "a DROP TABLE" },
  { pattern = "DROP%s+DATABASE", why = "a DROP DATABASE" },
  { pattern = "mkfs", why = "a filesystem format" },
  { pattern = ">%s*/dev/[sh]d", why = "a raw write to a disk device" },
}

bough.api.create_autocmd("PreTool", {
  pattern = "bash",
  callback = function(ev)
    local command = ev.data.input.command
    for _, rule in ipairs(DESTRUCTIVE) do
      if string.find(command, rule.pattern) then
        return {
          decision = "deny",
          reason = "guard-destructive refused " .. rule.why
            .. ". Ask the user to run it themselves, or take a reversible route.",
        }
      end
    end
  end,
})
