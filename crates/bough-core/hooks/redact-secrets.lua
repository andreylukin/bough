-- Keep obvious credentials out of the transcript.
--
-- Rewrites what a command PRINTED before the model reads it. The history row
-- keeps the real output — a redaction hook must not be able to rewrite the
-- record of what happened — so this narrows what reaches the context window,
-- not what happened.

local SECRETS = {
  "sk%-[%w]+",                        -- OpenAI-style keys
  "sk%-ant%-[%w%-]+",                 -- Anthropic keys
  "ghp_[%w]+",                        -- GitHub tokens
  "gho_[%w]+",
  "AKIA[%u%d]+",                      -- AWS access key ids
  "xox[baprs]%-[%w%-]+",              -- Slack tokens
  "eyJ[%w%-_]+%.[%w%-_]+%.[%w%-_]+",  -- JWTs
}

bough.api.create_autocmd("PostTool", {
  pattern = "bash",
  callback = function(ev)
    local out = ev.data.output
    if out == nil or out == "" then return end
    local redacted = out
    for _, pattern in ipairs(SECRETS) do
      redacted = string.gsub(redacted, pattern, "[redacted]")
    end
    if redacted ~= out then
      return { output = redacted }
    end
  end,
})
