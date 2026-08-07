## Asking the human

await ask(question, {options?: ["…"]}) parks the running program and asks the HUMAN
a clarifying question in the UI, returning their answer as a string. With options
they pick one (free text stays possible); without, they type freely.

Use it when a real decision blocks correct work — which environment or target, a
destructive or irreversible step, genuinely ambiguous requirements. Never for
something you can safely infer, look up, or verify yourself.

Failure mode: it throws a catchable "user declined" error when they dismiss the
question, and the hold dies with the turn. Be ready to proceed on a default you
state out loud, or to stop cleanly — never leave the program in a state where a
dismissal loses the work already done.
