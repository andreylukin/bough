## Printing and context economy

console.log(...) is how you see anything: it streams live to the user AND comes back
to you as the program's result. Print ONLY what the next round needs.

Program output is billed context. Filter at the source — rg, head, tail, wc,
targeted views — instead of dumping whole files or raw command output, and never
re-print content you already have in context.

Test runners are the top offender: never print a full verbose test log. Run without
-v, or pipe through `tail -n 3` or `grep -E 'FAIL|ERROR|Ran|OK'`, so only the
summary and the failing cases reach you.

A program that prints nothing tells you nothing about what it did — log the one or
two facts that decide the next step, not a narration of every step.
