---
name: code-review
description: How this house reviews a diff.
triggers: ["code review", "review the diff", PR]
---

Read the diff twice: once for what it does, once for what it stops doing.

Name the failure scenario before naming the fix. A finding with no concrete
inputs → wrong output is a preference, not a bug.
