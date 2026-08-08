# The terminal UI

```bash
bough              # start in the current directory
bough -w ~/code/x  # start new conversations in that directory
bough -r           # reopen this workspace's last conversation
```

The TUI is a view over the server. Closing it does not stop a running turn; reopening
reattaches. Subagents keep running when you quit.

## One panel

Everything that is not the conversation lives in a single panel with eight tabs. Each
has a direct-jump chord — pressing it opens the panel *on that tab*, so there is no
navigating.

| Chord | Tab | |
|---|---|---|
| `^f` | tree | conversations, turns, branches (`^s` also works) |
| `^d` | changes | what this session changed |
| `^w` | workflows | workflow runs |
| `^o` | model | frontier · cheap · thinking depth |
| `^p` | mcp | servers, grants, authorization |
| `^k` | skills | installed `/skills` |
| `^x` | hooks | Lua that runs in the loop; toggle it |
| `^y` | theme | browse live; leaving reverts |

`^t` opens and closes the panel without naming a tab. `tab` / `shift-tab` move between
tabs, `/` filters within one, `esc` goes back to chat.

Hooks is `^x` and not `^h` because `^h` *is* backspace (0x08) — the terminal delivers it
to the composer, and the tab would be unreachable. That was found by driving a real PTY.

## Composing

| | |
|---|---|
| `enter` | send — interjects while a turn is running |
| `meta-enter` | queue for after this turn |
| `^j` | newline |
| `tab` | accept the suggested next message |
| `^n` | start a fresh conversation |
| `^v` | attach a clipboard image |
| `^g` | copy this conversation's id |

`@` opens a file picker, `/` a skill picker. Readline editing works as you expect
(`^a` `^e` `^b` `^f` `^w` `^k` `^u`).

**Escape unwinds exactly one level, nearest surface first.** It is one key doing the
obvious thing in each context, in this order: dismiss the `@`/`/` popup → take back the
message you just sent (within 3 seconds) → stop the running turn → `esc` `esc` on an
empty draft to go back to a turn and fork it.

Full, current keymap: press `?`. It is generated from the same table the key handler
reads, so it cannot drift from the bindings.

## Reading

| | |
|---|---|
| `^e` | fold / unfold every tool call |
| `↓` | into the live work rail (from an empty draft) |
| `pageup` / `pagedown` | scroll back and forward |
| `↑` | message history |
| `esc` | from a subagent, back to the session that spawned it |

Unfolding a step shows the actual program that ran and its output. That is the honest
view of what bough did — not a summary of it.

## A session

Point it at a repo and ask in plain language. bough writes one program, runs it, and
answers. Reasoning folds away; cost and remaining context sit in the status bar.

**Review** with `^d`. The Changes rail is `git diff` against the sha the session started
from — per file, revertable per path, and never a staging area of its own. You commit
and push with your own git.

**Branch** with `esc` `esc` on an empty draft. Rewind to any turn, send something else, and the old line
survives as a branch in the tree (`^f`). Compacting a span or lifting messages into a
fresh root works the same way: a new branch, never a rewrite. Nothing is destroyed.

**Interject** by typing while a turn runs — `enter` reaches the model mid-turn.
`meta-enter` queues instead.
