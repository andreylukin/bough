---
name: theme
description: Design a UI color theme from a description and save it as JSON
---

Create a color theme for the bough TUI from the user's description (a mood,
some base colors, or a well-known palette like Rosé Pine / Nord / Gruvbox), save it via
the theme API, and show the JSON so it can be copied and shared. The bough API is at
http://127.0.0.1:${BOUGH_PORT:-4321}, reachable from your shell.

## 1. Ground yourself
`curl -s localhost:4321/theme` returns {theme, tokens, defaults}: the currently saved
theme (null = default), the fixed token contract, and the default palette. Draft against
these token semantics — the UI is a neutral-dark surface stack with ONE identity accent:
- bg: app background (darkest). canvas: the map/graph background, a hair off bg.
- panel < panel2 < panel3 < panelInset: elevation ramp for rails, cards, insets —
  keep them close siblings of bg (subtle steps, not contrasting blocks).
- border > border2 > border3: dividers strong→faint; hairline: the brightest edge.
- text > text2 > muted > muted2: text emphasis ramp, text must stay readable on bg
  (aim ≥ 7:1 contrast) and muted2 is the faintest legible tier.
- green: THE accent — running/success states, pulses, primary buttons. amber: pending /
  hold-and-ask. red: deny/danger. blue: links/info. Keep all four distinguishable;
  glows are derived from these automatically, so pick accents that read on bg.
If the user names a published palette, use its real values (fetch or recall them),
mapping its roles onto the tokens above — don't invent approximations when official
hex values exist.

## 2. Draft the JSON
{"name":"<display name>","colors":{"bg":"#191724", ... }}
Hex only (#rgb/#rrggbb/#rrggbbaa). colors may be partial — omitted tokens keep their
defaults — but a full 18-token palette themes cleanly; partial makes sense only for
accent-swap tweaks. Sanity-check yourself: text vs bg contrast, panels ordered by
elevation, accents distinct from each other and from the surfaces.

## 3. Save and prove it
PUT it: `curl -s -X PUT localhost:4321/theme -H 'content-type: application/json' -d @theme.json`
A 400 names the offending token — fix and retry. Then GET /theme again and confirm the
saved theme round-trips. To revert to the default palette: `curl -X DELETE localhost:4321/theme`.

## 4. Report
Show the final JSON in a fenced code block (that block IS the shareable artifact —
anyone can PUT it on their own bough), note any tokens left at defaults, and remind
the user to refresh the browser tab to see it applied.
