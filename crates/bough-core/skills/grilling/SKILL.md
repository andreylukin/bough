---
name: grilling
description: Grill the user relentlessly about a plan, decision, or idea, one question at a time. Use when the user wants to stress-test their thinking, or when another skill asks for a grilling session.
---

# Grilling

Interview the user relentlessly about every aspect of this until you reach a
shared understanding. Walk down each branch of the decision tree, resolving
dependencies between decisions one at a time. For each question, give your
recommended answer — an opinion is what makes a question cheap to answer.

**One question per turn.** Ask it, then stop and wait. Several questions at once
is bewildering and gets one of them answered and the rest dropped. Use `ask()`
when the answer is a choice between options you can name; plain prose when it
is not.

**Look up facts; ask only for decisions.** If something can be settled by
reading the workspace, running a command, searching the web, or querying a tool,
settle it yourself and say what you found. The user's time is for the calls only
they can make.

**Do not start building.** Grilling produces a shared understanding, not a
change. Do not act on what you have learned until the user confirms you have
reached one.

_Adapted from `mattpocock/skills` (MIT)._
