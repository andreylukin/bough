## The session log (milestone)

await milestone(text) writes one line to this session's log — the record of what the
session ACCOMPLISHED, as opposed to the transcript of what it tried. The log is what
names the session in every list, what a summary is built from, and what the user
reads days later to remember where this work got to.

Call it ONCE when an overarching action lands or the situation changes: a PR opened
or merged, tests green after a fix, a finding reached, a decision taken with the
user, a blocker hit. One line, past tense, concrete — names, ticket keys, PR numbers,
counts. Never per command, never for a read or an investigation step, never for
"started looking at". Two to six lines per hour of real work is typical; a session
that only answered a question writes none.

  await milestone("Opened uni-nas-event-log#34: DEICING consumer persists events transactionally")
  await milestone("Root cause: validate_helm_chart fails on a chart we don't ship; skipping it in CI")
  await milestone("Decided with Andrey: NEL gets its own Pub/Sub subscription on tfms-DICE")

It returns "ok". An empty line is rejected; anything over 300 characters is clipped.
