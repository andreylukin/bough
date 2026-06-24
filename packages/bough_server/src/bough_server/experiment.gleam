//// Offload-spectrum experiment harness (SPEC §5 R&D).
////
//// Question: how much can the supervisor offload to the small worker model
//// (VibeThinker-3B), and where does it get overloaded? The cleanest lever is to
//// run the 3B *as the supervisor* in the real engine loop — the engine is
//// provider-agnostic (`provider.OpenAICompat`), so this needs no change to the
//// loop — and sweep the autonomy/budget axis (`max_rounds`) against a graded
//// task suite. The overload frontier is where the worker's success rate falls
//// off as task difficulty rises faster than added rounds can recover.
////
//// Measurement discipline: success is an INDEPENDENT ground-truth check we run
//// after the engine returns, never the model's own committed `check` — a weak
//// model writes weak checks, so trusting them would measure the wrong thing.
////
//// Run:  cd packages/bough_server && gleam run -m bough_server/experiment
//// Knobs (env):
////   BOUGH_EXP_WORKER_URL    worker base (default http://127.0.0.1:8080)
////   BOUGH_EXP_WORKER_MODEL  worker model name (default vibethinker-3b)
////   BOUGH_EXP_ROUNDS        comma round-caps to sweep (default 1,3,6,12)
////   BOUGH_EXP_TRIALS        repeats per (task,cell) (default 1)
////   BOUGH_EXP_TASKS         comma task-name filter (default all)
////   BOUGH_EXP_BASELINE=1    also run an Anthropic baseline (needs
////                           ANTHROPIC_API_KEY; model BOUGH_EXP_BASELINE_MODEL,
////                           default claude-haiku-4-5)

import bough_core/artifact
import bough_core/digest
import bough_core/nono
import bough_server/agent.{
  type Step, StepCheck, StepExec, StepReview, StepText, StepWorker,
}
import bough_server/anthropic
import bough_server/clock
import bough_server/engine
import bough_server/monty_bridge
import bough_server/nono_bridge
import bough_server/provider
import bough_server/tools
import bough_server/worker
import envoy
import gleam/int
import gleam/io
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import shellout
import simplifile

// --- Spectrum cells & task suite -----------------------------------------

/// One point on the offload spectrum: the same engine loop, a chosen supervisor
/// provider/model, and an autonomy budget (round cap). `max_steps` scales with
/// rounds so a short-budget cell isn't also starved of actions within a round.
type Cell {
  Cell(
    label: String,
    provider: provider.Provider,
    api_key: String,
    model: String,
    max_rounds: Int,
  )
}

/// A graded task: a self-contained workspace (relative path → contents), the
/// prompt the supervisor receives, and `ground_truth` — a shell command run in
/// the finished workspace that exits 0 iff the task is genuinely solved. This is
/// OUR grader, independent of whatever `check` the model committed.
type Task {
  Task(
    name: String,
    difficulty: Int,
    fixtures: List(#(String, String)),
    prompt: String,
    ground_truth: String,
  )
}

/// Five difficulty tiers, three tasks each — a gradient from located one-line
/// fixes to multi-criterion features, with varied bug types (typo, wrong
/// operator, off-by-one, missing case) and a multi-file discover task.
/// Deterministic, no network, python3-only so the sandbox runs them and the
/// ground-truth check is exact (int/string/bool/exception-valued — no float
/// equality). Every check is QA'd to fail on the fixture and pass on a correct
/// solution. Extend freely; the harness sweeps every task × cell × trial.
fn suite() -> List(Task) {
  [
    // --- Tier 1: located, mechanical one-liners ---
    Task(
      name: "t1_typo",
      difficulty: 1,
      fixtures: [#("greeting.py", "def greet():\n    return \"Helo, world!\"\n")],
      prompt: "The function greet() in greeting.py should return the string 'Hello, world!' but it has a typo. Fix it.",
      ground_truth: "python3 -c \"import greeting; assert greeting.greet()=='Hello, world!'\"",
    ),
    Task(
      name: "t1_even",
      difficulty: 1,
      fixtures: [#("parity.py", "def is_even(n):\n    return n % 2 == 1\n")],
      prompt: "is_even(n) in parity.py should return True when n is even, but it returns the opposite. Fix it.",
      ground_truth: "python3 -c \"import parity as p; assert p.is_even(4); assert not p.is_even(3); assert p.is_even(0)\"",
    ),
    Task(
      name: "t1_week",
      difficulty: 1,
      fixtures: [#("calendar_util.py", "def days_in_week():\n    return 5\n")],
      prompt: "days_in_week() in calendar_util.py returns the wrong number — a week has 7 days. Fix it.",
      ground_truth: "python3 -c \"import calendar_util as c; assert c.days_in_week()==7\"",
    ),
    // --- Tier 2: implement a function from a spec ---
    Task(
      name: "t2_implement",
      difficulty: 2,
      fixtures: [#("mathutil.py", "def fib(n):\n    pass  # TODO: implement\n")],
      prompt: "Implement fib(n) in mathutil.py: it returns the nth Fibonacci number, with fib(0)=0 and fib(1)=1.",
      ground_truth: "python3 -c \"import mathutil as m; assert [m.fib(i) for i in range(8)]==[0,1,1,2,3,5,8,13]\"",
    ),
    Task(
      name: "t2_factorial",
      difficulty: 2,
      fixtures: [#("factorial.py", "def factorial(n):\n    pass  # TODO\n")],
      prompt: "Implement factorial(n) in factorial.py: factorial(0)=1 and factorial(n)=n*factorial(n-1) for n>0.",
      ground_truth: "python3 -c \"import factorial as f; assert [f.factorial(i) for i in range(6)]==[1,1,2,6,24,120]\"",
    ),
    Task(
      name: "t2_reverse",
      difficulty: 2,
      fixtures: [#("textutil.py", "def reverse_words(s):\n    pass  # TODO\n")],
      prompt: "Implement reverse_words(s) in textutil.py: return the space-separated words of s in reverse order. Example: reverse_words('a b c') == 'c b a'.",
      ground_truth: "python3 -c \"import textutil as t; assert t.reverse_words('a b c')=='c b a'; assert t.reverse_words('hi')=='hi'\"",
    ),
    // --- Tier 3: debug a subtle bug (located) ---
    Task(
      name: "t3_debug",
      difficulty: 3,
      fixtures: [#("slicer.py", "def last_n(xs, n):\n    return xs[n:]\n")],
      prompt: "last_n(xs, n) in slicer.py should return the LAST n elements of xs, but it's buggy. Example: last_n([1,2,3,4,5], 2) must be [4,5], and last_n(xs, 0) must be []. Fix it.",
      ground_truth: "python3 -c \"from slicer import last_n; assert last_n([1,2,3,4,5],2)==[4,5]; assert last_n([1,2,3],0)==[]; assert last_n([1,2,3],5)==[1,2,3]\"",
    ),
    Task(
      name: "t3_max",
      difficulty: 3,
      fixtures: [
        #(
          "maxutil.py",
          "def max_of(xs):\n    m = xs[0]\n    for x in xs:\n        if x < m:\n            m = x\n    return m\n",
        ),
      ],
      prompt: "max_of(xs) in maxutil.py should return the LARGEST element of xs, but it returns the smallest. Fix it. Example: max_of([3,1,4,1,5]) == 5.",
      ground_truth: "python3 -c \"from maxutil import max_of; assert max_of([3,1,4,1,5,9,2,6])==9; assert max_of([-3,-1,-2])==-1\"",
    ),
    Task(
      name: "t3_vowels",
      difficulty: 3,
      fixtures: [
        #(
          "vowelcount.py",
          "def count_vowels(s):\n    vowels = \"aeio\"\n    return sum(1 for c in s if c in vowels)\n",
        ),
      ],
      prompt: "count_vowels(s) in vowelcount.py should count vowels (a, e, i, o, u) case-insensitively, but it's missing some. Fix it. Example: count_vowels('Education') == 5.",
      ground_truth: "python3 -c \"from vowelcount import count_vowels; assert count_vowels('Education')==5; assert count_vowels('RHYTHM')==0; assert count_vowels('AEIOU')==5\"",
    ),
    // --- Tier 4: discover-and-fix, no location given (some multi-file) ---
    Task(
      name: "t4_discover",
      difficulty: 4,
      fixtures: [
        #("app/__init__.py", ""),
        #(
          "app/calc.py",
          "def total(items):\n    # each item: {'price': float, 'qty': int}\n    return sum(i['price'] for i in items)\n",
        ),
        #(
          "main.py",
          "from app.calc import total\nprint(total([{'price': 2.0, 'qty': 3}]))\n",
        ),
      ],
      prompt: "Orders are totaled incorrectly: the total ignores quantity. Each item is {'price': float, 'qty': int} and the total should sum price*qty across items. Find and fix the bug.",
      ground_truth: "python3 -c \"from app.calc import total; assert total([{'price':2.0,'qty':3},{'price':1.5,'qty':2}])==9.0; assert total([])==0\"",
    ),
    Task(
      name: "t4_slugify",
      difficulty: 4,
      fixtures: [
        #("lib/__init__.py", ""),
        #("lib/strings.py", "def slugify(s):\n    return s.replace(\" \", \"-\")\n"),
        #("main.py", "from lib.strings import slugify\nprint(slugify(\"Hello World\"))\n"),
      ],
      prompt: "Slugs come out with uppercase letters. slugify() should return a lowercase, dash-separated slug. Find and fix it.",
      ground_truth: "python3 -c \"from lib.strings import slugify; assert slugify('Hello World')=='hello-world'; assert slugify('A B C')=='a-b-c'\"",
    ),
    Task(
      name: "t4_discount_sign",
      difficulty: 4,
      fixtures: [
        #("store/__init__.py", ""),
        #("store/pricing.py", "def net(price, discount):\n    return price + discount\n"),
        #("app.py", "from store.pricing import net\nprint(net(100, 30))\n"),
      ],
      prompt: "Applying a discount is increasing the price instead of reducing it. net(price, discount) should subtract the discount from the price. Find and fix the bug.",
      ground_truth: "python3 -c \"from store.pricing import net; assert net(100,30)==70; assert net(50,0)==50\"",
    ),
    // --- Tier 5: multi-criterion features ---
    Task(
      name: "t5_feature",
      difficulty: 5,
      fixtures: [#("store.py", "# Pricing helpers.\n")],
      prompt: "In store.py add two functions. discount(price, pct): return price reduced by pct percent, rounded to 2 decimals; raise ValueError if pct is not in the range 0..100. apply_all(prices, pct): return a list of the discounted prices (using discount).",
      ground_truth: "python3 -c \"import store\nassert store.discount(100,10)==90.0\nassert store.discount(99.99,10)==89.99\nassert store.apply_all([100,200],50)==[50.0,100.0]\ntry:\n    store.discount(10,150); raise SystemExit(1)\nexcept ValueError:\n    pass\"",
    ),
    Task(
      name: "t5_counter",
      difficulty: 5,
      fixtures: [#("counter.py", "# Implement a Counter class.\n")],
      prompt: "In counter.py implement a class Counter: it starts at 0; inc(n=1) adds n (default 1) to the count; value() returns the current count; reset() sets it back to 0.",
      ground_truth: "python3 -c \"from counter import Counter\nc = Counter()\nc.inc()\nc.inc(3)\nassert c.value()==4\nc.reset()\nassert c.value()==0\"",
    ),
    Task(
      name: "t5_brackets",
      difficulty: 5,
      fixtures: [#("brackets.py", "def balanced(s):\n    pass  # TODO\n")],
      prompt: "Implement balanced(s) in brackets.py: return True iff the brackets (), [], {} in s are correctly balanced and nested. Non-bracket characters are ignored. The empty string is balanced.",
      ground_truth: "python3 -c \"from brackets import balanced; assert balanced('([]{})'); assert not balanced('([)]'); assert balanced(''); assert not balanced('(((')\"",
    ),
  ]
}

// --- Result record --------------------------------------------------------

type Metric {
  Metric(
    task: String,
    difficulty: Int,
    cell: String,
    trial: Int,
    // The thing we actually care about: did our independent check pass?
    solved: Bool,
    // How it stopped — the overload signal lives here.
    outcome: String,
    rounds: Int,
    exec_steps: Int,
    worker_fixes: Int,
    model_check_committed: Bool,
    model_check_passed: Bool,
    context_tokens: Int,
    wall_ms: Int,
  )
}

// --- Driver ---------------------------------------------------------------

/// Two experiments. Default ("worker") characterizes the 3B WORKER's frontier:
/// the supervisor is the capable model (Anthropic), the worker is the small
/// local 3B, and we sweep how much autonomy the supervisor offloads to it. The
/// "supervisor" mode (BOUGH_EXP_MODE=supervisor) is the earlier study of running
/// a model AS the supervisor — kept for reference.
pub fn main() -> Nil {
  case envoy.get("BOUGH_EXP_MODE") {
    Ok("supervisor") -> supervisor_sweep()
    Ok("relay") -> relay_sweep()
    Ok("escalate") -> escalate_sweep()
    Ok("line") -> line_sweep()
    _ -> worker_sweep()
  }
}

fn supervisor_sweep() -> Nil {
  let cells = cells_from_env()
  let tasks = filtered_tasks()
  let trials = env_int("BOUGH_EXP_TRIALS", 1)
  let jsonl = open_jsonl()

  io.println(
    "Supervisor-mode sweep: "
    <> int.to_string(list.length(tasks))
    <> " tasks × "
    <> int.to_string(list.length(cells))
    <> " cells × "
    <> int.to_string(trials)
    <> " trial(s)\n→ "
    <> jsonl
    <> "\n",
  )

  let metrics =
    list.flat_map(cells, fn(cell) {
      list.flat_map(tasks, fn(task) {
        range(1, trials)
        |> list.map(fn(trial) {
          let m = run_one(cell, task, trial)
          let _ = simplifile.append(jsonl, metric_json(m) <> "\n")
          io.println(line(m))
          m
        })
      })
    })

  io.println("\n" <> summary(metrics, list.map(cells, fn(c) { c.label }), tasks))
}

fn open_jsonl() -> String {
  let _ = simplifile.create_directory_all(home() <> "/.bough/experiments")
  home() <> "/.bough/experiments/results-" <> int.to_string(clock.now_ms()) <> ".jsonl"
}

// --- Worker-frontier mode -------------------------------------------------

/// One offload level: how much autonomy the supervisor hands the worker. The
/// worker speaks plain text (fenced blocks) — NOT structured tool calls — so a
/// 3B coder model is a faithful worker regardless of tool-call support.
/// `primitives` selects the action channel: raw shell (`W*`) vs. structured
/// write/edit/run blocks (`P*`), to separate reasoning from shell-edit friction.
type Level {
  Level(label: String, system: String, max_iters: Int, primitives: Bool)
}

const prim_system = "You are a coding worker working in the current workspace directory. Act using these fenced blocks (and nothing else):

```write <path>
<full new file contents>
```
overwrites/creates the file at <path> with the block body.

```edit <path>
<<<<<<< SEARCH
<exact text to find>
=======
<replacement text>
>>>>>>> REPLACE
```
replaces the first exact occurrence of SEARCH with REPLACE in <path>.

```sh
<command>
```
runs a non-interactive shell command (for inspecting or verifying).

Prefer `write` (give the whole file) over `edit` when unsure. Emit only the blocks needed."

fn worker_levels() -> List(Level) {
  [
    // W1: the narrowest offload — accomplish the whole task in one command batch.
    Level(
      "W1-oneshot",
      "You are a coding worker on a macOS machine, working in the current workspace directory. Accomplish the task using shell command(s). Respond with ONLY a single fenced ```sh code block (chain commands with &&). Commands run non-interactively — no editors or prompts.",
      1,
      False,
    ),
    // W2: an exploratory ask — reason first, then act once.
    Level(
      "W2-reason",
      "You are a coding worker on a macOS machine, working in the current workspace directory. Think step by step about what the task needs, then give the commands to accomplish it as ONE fenced ```sh code block at the end of your reply (chain with &&). Commands run non-interactively.",
      1,
      False,
    ),
    // W3: a full agent subloop — iterate against live output until done.
    Level(
      "W3-subloop",
      "You are a coding worker operating a macOS terminal in the workspace directory. Work toward the task ONE step at a time. Each turn, reply with exactly one fenced ```sh code block to run next; you will see its output and may continue. When the task is fully complete, reply with the single word DONE and no code block. Commands run non-interactively.",
      6,
      False,
    ),
    // P1: one-shot with structured write/edit primitives (vs W1's raw shell).
    Level(
      label: "P1-prim-oneshot",
      system: prim_system,
      max_iters: 1,
      primitives: True,
    ),
    // P3: subloop with primitives — the worker may inspect, then write/edit.
    Level(
      label: "P3-prim-subloop",
      system: prim_system
        <> "\n\nWork one turn at a time; you will see each block's result and may continue. Reply with the single word DONE (no blocks) when the task is complete.",
      max_iters: 6,
      primitives: True,
    ),
  ]
}

fn worker_sweep() -> Nil {
  let url =
    envoy.get("BOUGH_EXP_WORKER_URL") |> result.unwrap("http://127.0.0.1:11434")
  let model =
    envoy.get("BOUGH_EXP_WORKER_MODEL") |> result.unwrap("qwen2.5-coder:3b")
  let tasks = filtered_tasks()
  let trials = env_int("BOUGH_EXP_TRIALS", 1)
  let levels = case envoy.get("BOUGH_EXP_LEVELS") {
    Error(_) -> worker_levels()
    Ok(spec) -> {
      let want = spec |> string.split(",") |> list.map(string.trim)
      list.filter(worker_levels(), fn(l) { list.contains(want, l.label) })
    }
  }
  let jsonl = open_jsonl()

  io.println(
    "Worker-frontier sweep: worker="
    <> model
    <> " @ "
    <> url
    <> "\n"
    <> int.to_string(list.length(tasks))
    <> " tasks × "
    <> int.to_string(list.length(levels))
    <> " offload levels × "
    <> int.to_string(trials)
    <> " trial(s)\n→ "
    <> jsonl
    <> "\n",
  )

  let metrics =
    list.flat_map(levels, fn(level) {
      list.flat_map(tasks, fn(task) {
        range(1, trials)
        |> list.map(fn(trial) {
          let m = run_worker_one(url, model, level, task, trial)
          let _ = simplifile.append(jsonl, metric_json(m) <> "\n")
          io.println(line(m))
          m
        })
      })
    })

  io.println(
    "\n" <> summary(metrics, list.map(levels, fn(l) { l.label }), tasks),
  )
}

/// Delegate one task to the worker at one offload level: fresh workspace, run the
/// worker (one shot, or a bounded subloop), then grade with the independent
/// ground-truth check.
fn run_worker_one(
  url: String,
  model: String,
  level: Level,
  task: Task,
  trial: Int,
) -> Metric {
  let ws =
    home()
    <> "/.bough/experiments/ws/"
    <> task.name
    <> "__"
    <> level.label
    <> "__t"
    <> int.to_string(trial)
    <> "_"
    <> int.to_string(clock.now_ms())
  let _ = simplifile.create_directory_all(ws)
  write_fixtures(ws, task.fixtures)
  // Hand the worker the workspace file contents — what a supervisor offloading
  // a sub-task would include — so we measure reasoning, not blind guessing.
  let files_block =
    task.fixtures
    |> list.map(fn(f) { "=== " <> f.0 <> " ===\n" <> f.1 })
    |> string.join("\n")
  let user =
    "Files in the workspace:\n" <> files_block <> "\nTask: " <> task.prompt

  let t0 = clock.now_ms()
  let #(iters, execs, outcome) = case level.primitives {
    True -> prim_loop(url, model, level, ws, task, user, 1, 0)
    False -> worker_loop(url, model, level, ws, task, user, 1, 0)
  }
  let wall = clock.now_ms() - t0
  let solved = check_passes(ws, task.ground_truth)
  Metric(
    task: task.name,
    difficulty: task.difficulty,
    cell: level.label,
    trial: trial,
    solved: solved,
    outcome: outcome,
    rounds: iters,
    exec_steps: execs,
    worker_fixes: 0,
    model_check_committed: False,
    model_check_passed: False,
    context_tokens: 0,
    wall_ms: wall,
  )
}

fn worker_loop(
  url: String,
  model: String,
  level: Level,
  ws: String,
  task: Task,
  user: String,
  iter: Int,
  execs: Int,
) -> #(Int, Int, String) {
  case iter > level.max_iters {
    True -> #(iter - 1, execs, "exhausted")
    False ->
      case worker.complete(url, model, level.system, user, 1500) {
        Error(_) -> #(iter, execs, "worker_error")
        Ok(text) ->
          case artifact.first_fence(text) {
            // No command this turn. In a subloop a bare "DONE" is a clean stop;
            // otherwise the worker produced no actionable command.
            None ->
              case level.max_iters > 1 && string.contains(text, "DONE") {
                True -> #(iter, execs, "worker_done")
                False -> #(iter, execs, "no_command")
              }
            Some(cmd) -> {
              let profile = nono.Profile(ws, [], True, False)
              let out = nono_bridge.run(profile, ["sh", "-c", cmd])
              let execs = execs + 1
              continue_or_finish(url, model, level, ws, task, user, iter, execs, cmd, out)
            }
          }
      }
  }
}

/// The primitive channel: parse the worker's reply into write/edit/run ops and
/// apply them in order. Returns the same #(iters, execs, outcome) shape so the
/// matrix is comparable to the shell levels.
fn prim_loop(
  url: String,
  model: String,
  level: Level,
  ws: String,
  task: Task,
  user: String,
  iter: Int,
  execs: Int,
) -> #(Int, Int, String) {
  case iter > level.max_iters {
    True -> #(iter - 1, execs, "exhausted")
    False ->
      case worker.complete(url, model, level.system, user, 4096) {
        Error(_) -> #(iter, execs, "worker_error")
        Ok(text) ->
          case parse_ops(text) {
            [] ->
              case level.max_iters > 1 && string.contains(text, "DONE") {
                True -> #(iter, execs, "worker_done")
                False -> #(iter, execs, "no_command")
              }
            ops -> {
              let out = apply_ops(ws, ops)
              let execs = execs + list.length(ops)
              continue_or_finish(
                url, model, level, ws, task, user, iter, execs,
                ops_summary(ops), out,
              )
            }
          }
      }
  }
}

/// Shared tail for both channels: grade; if unsolved, either feed the
/// action + output back (subloop) or stop (one-shot).
fn continue_or_finish(
  url: String,
  model: String,
  level: Level,
  ws: String,
  task: Task,
  user: String,
  iter: Int,
  execs: Int,
  action: String,
  out: String,
) -> #(Int, Int, String) {
  case check_passes(ws, task.ground_truth) {
    True -> #(iter, execs, "solved@" <> int.to_string(iter))
    False ->
      case level.max_iters > 1 {
        True -> {
          let next =
            user
            <> "\n\n[ran] "
            <> action
            <> "\n"
            <> digest.digest(out, 800)
            <> "\n\nContinue, or reply DONE if complete."
          case level.primitives {
            True -> prim_loop(url, model, level, ws, task, next, iter + 1, execs)
            False -> worker_loop(url, model, level, ws, task, next, iter + 1, execs)
          }
        }
        False -> #(iter, execs, "unsolved")
      }
  }
}

// --- Line mode: how specific must the supervisor's instruction be? ---------

/// One task at four instruction-specificity rungs (the worker always sees the
/// file contents; only the instruction varies). A single worker shot per rung —
/// faithful to bough's stateless `fix_loop` — locates where success collapses
/// as the supervisor generalizes. Fixtures/checks reuse the QA'd suites.
type LineTask {
  LineTask(
    name: String,
    difficulty: Int,
    fixtures: List(#(String, String)),
    ground_truth: String,
    // S0 dictated → S1 located → S2 behavior → S3 symptom.
    s0_dictated: String,
    s1_located: String,
    s2_behavior: String,
    s3_symptom: String,
  )
}

fn line_tasks() -> List(LineTask) {
  [
    LineTask(
      name: "slicer",
      difficulty: 3,
      fixtures: [#("slicer.py", "def last_n(xs, n):\n    return xs[n:]\n")],
      ground_truth: "python3 -c \"from slicer import last_n; assert last_n([1,2,3,4,5],2)==[4,5]; assert last_n([1,2,3],0)==[]; assert last_n([1,2,3],5)==[1,2,3]\"",
      s0_dictated: "In slicer.py, change the body of last_n to: return xs[-n:] if n > 0 else []",
      s1_located: "In slicer.py, last_n(xs, n) should return the LAST n elements of xs (last_n([1,2,3,4,5],2)==[4,5], last_n(xs,0)==[]) but it's buggy. Fix it.",
      s2_behavior: "A function is supposed to return the last n elements of a list but returns the wrong slice. Fix it.",
      s3_symptom: "Slicing a list is returning the wrong elements. Fix it.",
    ),
    LineTask(
      name: "max",
      difficulty: 3,
      fixtures: [
        #(
          "maxutil.py",
          "def max_of(xs):\n    m = xs[0]\n    for x in xs:\n        if x < m:\n            m = x\n    return m\n",
        ),
      ],
      ground_truth: "python3 -c \"from maxutil import max_of; assert max_of([3,1,4,1,5,9,2,6])==9; assert max_of([-3,-1,-2])==-1\"",
      s0_dictated: "In maxutil.py, change the comparison in max_of from `if x < m` to `if x > m`.",
      s1_located: "In maxutil.py, max_of(xs) should return the largest element of xs but returns the smallest. Fix it (max_of([3,1,4,1,5])==5).",
      s2_behavior: "A function meant to find the maximum of a list is returning the minimum instead. Fix it.",
      s3_symptom: "The max calculation is wrong. Fix it.",
    ),
    LineTask(
      name: "vowels",
      difficulty: 3,
      fixtures: [
        #(
          "vowelcount.py",
          "def count_vowels(s):\n    vowels = \"aeio\"\n    return sum(1 for c in s if c in vowels)\n",
        ),
      ],
      ground_truth: "python3 -c \"from vowelcount import count_vowels; assert count_vowels('Education')==5; assert count_vowels('RHYTHM')==0; assert count_vowels('AEIOU')==5\"",
      s0_dictated: "In vowelcount.py, change vowels to 'aeiou' and count over s.lower(), so 'u' is included and it's case-insensitive.",
      s1_located: "In vowelcount.py, count_vowels(s) should count a,e,i,o,u case-insensitively but misses some (count_vowels('Education')==5). Fix it.",
      s2_behavior: "A vowel counter is undercounting: it misses one of the vowels and ignores letter case. Fix it.",
      s3_symptom: "The vowel count comes out wrong. Fix it.",
    ),
    LineTask(
      name: "fib",
      difficulty: 2,
      fixtures: [#("mathutil.py", "def fib(n):\n    pass  # TODO\n")],
      ground_truth: "python3 -c \"import mathutil as m; assert [m.fib(i) for i in range(8)]==[0,1,1,2,3,5,8,13]\"",
      s0_dictated: "In mathutil.py, implement fib iteratively: set a,b=0,1; loop n times doing a,b=b,a+b; return a.",
      s1_located: "In mathutil.py, implement fib(n) returning the nth Fibonacci number, with fib(0)=0 and fib(1)=1.",
      s2_behavior: "Implement the Fibonacci function in mathutil.py (fib(0)=0, fib(1)=1, each is the sum of the previous two).",
      s3_symptom: "Complete the unimplemented function in mathutil.py.",
    ),
    LineTask(
      name: "total",
      difficulty: 4,
      fixtures: [
        #("app/__init__.py", ""),
        #(
          "app/calc.py",
          "def total(items):\n    # each item: {'price': float, 'qty': int}\n    return sum(i['price'] for i in items)\n",
        ),
        #("main.py", "from app.calc import total\n"),
      ],
      ground_truth: "python3 -c \"from app.calc import total; assert total([{'price':2.0,'qty':3},{'price':1.5,'qty':2}])==9.0; assert total([])==0\"",
      s0_dictated: "In app/calc.py, change `sum(i['price'] for i in items)` to `sum(i['price'] * i['qty'] for i in items)`.",
      s1_located: "In app/calc.py, total(items) ignores quantity; each item is {'price','qty'} and the total should sum price*qty. Fix it.",
      s2_behavior: "Order totals ignore quantity — each item has a price and a qty, and the total should multiply them. Fix it.",
      s3_symptom: "Order totals are coming out too low. Fix it.",
    ),
    LineTask(
      name: "discount",
      difficulty: 4,
      fixtures: [
        #("store/__init__.py", ""),
        #("store/pricing.py", "def net(price, discount):\n    return price + discount\n"),
        #("app.py", "from store.pricing import net\n"),
      ],
      ground_truth: "python3 -c \"from store.pricing import net; assert net(100,30)==70; assert net(50,0)==50\"",
      s0_dictated: "In store/pricing.py, change `return price + discount` to `return price - discount`.",
      s1_located: "In store/pricing.py, net(price, discount) adds the discount instead of subtracting it. Fix it (net(100,30)==70).",
      s2_behavior: "Applying a discount is increasing the price instead of reducing it. Fix it.",
      s3_symptom: "Discounts aren't working right. Fix it.",
    ),
    LineTask(
      name: "intervals",
      difficulty: 6,
      fixtures: [
        #(
          "intervals.py",
          "def merge(intervals):\n    res=[]\n    for s,e in intervals:\n        if res and s==res[-1][1]:\n            res[-1][1]=e\n        else:\n            res.append([s,e])\n    return res\n",
        ),
      ],
      ground_truth: "python3 -c \"from intervals import merge\nassert merge([[1,3],[2,6],[8,10],[15,18]])==[[1,6],[8,10],[15,18]]\nassert merge([[1,4],[4,5]])==[[1,5]]\nassert merge([[5,6],[1,2]])==[[1,2],[5,6]]\"",
      s0_dictated: "In intervals.py, rewrite merge to: sort the intervals by start; then iterate, and when the next start <= the last end, extend the last end to max(last_end, this_end), otherwise append a new [s,e].",
      s1_located: "In intervals.py, merge(intervals) should merge all overlapping intervals and return them sorted by start, but it only joins exactly-touching, already-sorted ones. Fix it (merge([[1,3],[2,6]])==[[1,6]]).",
      s2_behavior: "Overlapping intervals aren't being merged correctly: only exactly-adjacent ones get combined, and unsorted input breaks it. Fix it.",
      s3_symptom: "The interval merging is buggy. Fix it.",
    ),
    LineTask(
      name: "roman",
      difficulty: 7,
      fixtures: [#("roman.py", "def to_roman(n):\n    pass  # TODO\n")],
      ground_truth: "python3 -c \"from roman import to_roman\nassert to_roman(4)=='IV'\nassert to_roman(58)=='LVIII'\nassert to_roman(1994)=='MCMXCIV'\nassert to_roman(3888)=='MMMDCCCLXXXVIII'\"",
      s0_dictated: "In roman.py, implement to_roman with a descending list of (value, symbol) pairs [(1000,'M'),(900,'CM'),(500,'D'),(400,'CD'),(100,'C'),(90,'XC'),(50,'L'),(40,'XL'),(10,'X'),(9,'IX'),(5,'V'),(4,'IV'),(1,'I')], greedily appending symbols while subtracting value from n.",
      s1_located: "In roman.py, implement to_roman(n) for 1<=n<=3999 using standard subtractive notation (4=IV, 9=IX, 40=XL, 90=XC, 400=CD, 900=CM). Example: to_roman(1994)=='MCMXCIV'.",
      s2_behavior: "Implement to_roman(n) in roman.py, converting an integer in 1..3999 to its Roman numeral string.",
      s3_symptom: "Complete the unimplemented function in roman.py.",
    ),
  ]
}

fn line_sweep() -> Nil {
  let url =
    envoy.get("BOUGH_EXP_WORKER_URL")
    |> result.unwrap("http://127.0.0.1:11434")
  let model =
    envoy.get("BOUGH_EXP_WORKER_MODEL") |> result.unwrap("qwen2.5-coder:3b")
  let trials = env_int("BOUGH_EXP_TRIALS", 1)
  let tasks = line_tasks()
  let jsonl = open_jsonl()

  io.println(
    "Line sweep (instruction specificity → single worker shot): worker="
    <> model
    <> "\n"
    <> int.to_string(list.length(tasks))
    <> " tasks × 4 rungs × "
    <> int.to_string(trials)
    <> " trial(s)\n→ "
    <> jsonl
    <> "\n",
  )

  let metrics =
    list.flat_map(tasks, fn(task) {
      let rungs = [
        #("S0-dictated", task.s0_dictated),
        #("S1-located", task.s1_located),
        #("S2-behavior", task.s2_behavior),
        #("S3-symptom", task.s3_symptom),
      ]
      list.flat_map(rungs, fn(rung) {
        range(1, trials)
        |> list.map(fn(trial) {
          let m = run_line_one(url, model, task, rung.0, rung.1, trial)
          let _ = simplifile.append(jsonl, metric_json(m) <> "\n")
          io.println(line(m))
          m
        })
      })
    })

  io.println("\n" <> level_summary(metrics))
}

/// One single-shot delegation: the worker sees the files + this rung's
/// instruction, acts once (primitives), then we grade.
fn run_line_one(
  url: String,
  model: String,
  task: LineTask,
  rung: String,
  instruction: String,
  trial: Int,
) -> Metric {
  let ws =
    home()
    <> "/.bough/experiments/ws/line_"
    <> task.name
    <> "__"
    <> rung
    <> "__t"
    <> int.to_string(trial)
    <> "_"
    <> int.to_string(clock.now_ms())
  let _ = simplifile.create_directory_all(ws)
  write_fixtures(ws, task.fixtures)
  let user =
    "Files in the workspace:\n"
    <> current_files(ws, task.fixtures)
    <> "\n\nInstruction: "
    <> instruction
  let t0 = clock.now_ms()
  let #(n, _summary, _out) = apply_executor(url, model, ws, user)
  let wall = clock.now_ms() - t0
  let solved = check_passes(ws, task.ground_truth)
  Metric(
    task: task.name,
    difficulty: task.difficulty,
    cell: rung,
    trial: trial,
    solved: solved,
    outcome: case n {
      0 -> "no_action"
      _ ->
        case solved {
          True -> "solved"
          False -> "miss"
        }
    },
    rounds: 1,
    exec_steps: n,
    worker_fixes: 0,
    model_check_committed: False,
    model_check_passed: False,
    context_tokens: 0,
    wall_ms: wall,
  )
}

/// The headline: solve rate per specificity rung, overall and split by
/// easy (d<=3) vs hard (d>=4) — so the "line" (where rate falls off) is visible.
fn level_summary(metrics: List(Metric)) -> String {
  let rungs = ["S0-dictated", "S1-located", "S2-behavior", "S3-symptom"]
  let rows =
    list.map(rungs, fn(r) {
      let ms = list.filter(metrics, fn(m) { m.cell == r })
      let easy = list.filter(ms, fn(m) { m.difficulty <= 3 })
      let hard = list.filter(ms, fn(m) { m.difficulty >= 4 })
      pad(r, 14)
      <> "overall "
      <> pad(rate(ms), 8)
      <> " easy(d≤3) "
      <> pad(rate(easy), 8)
      <> " hard(d≥4) "
      <> rate(hard)
    })
  "Solve rate by instruction specificity (one worker shot):\n"
  <> string.join(rows, "\n")
  <> "\n\nThe line: the rung where the rate falls off is where the supervisor must stop generalizing."
}

// --- Escalation mode: fast executor first, relay only on CHECK failure ----

/// The daily-use shape: try the cheap fast path first (qwen one-shot, ~sub-
/// second), gate on CHECK, escalate to ONE local reasoner→executor relay round
/// on failure, and — only if that also fails — hand off to the frontier
/// supervisor (haiku) as the backstop. Tight local caps so we reach the frontier
/// fast instead of grinding the local stack; p50 is the fast tier, the frontier
/// catches the local ceiling. Reports per-tier solve + latency spread.
const escalate_relay_iters = 1

fn escalate_sweep() -> Nil {
  let ollama = "http://127.0.0.1:11434"
  let reasoner_url = envoy.get("BOUGH_EXP_REASONER_URL") |> result.unwrap(ollama)
  let reasoner_model =
    envoy.get("BOUGH_EXP_REASONER_MODEL")
    |> result.unwrap("hf.co/mradermacher/VibeThinker-3B-GGUF:Q4_K_M")
  let exec_url = envoy.get("BOUGH_EXP_WORKER_URL") |> result.unwrap(ollama)
  let exec_model =
    envoy.get("BOUGH_EXP_WORKER_MODEL") |> result.unwrap("qwen2.5-coder:3b")
  let sup_key = envoy.get("ANTHROPIC_API_KEY") |> result.unwrap("")
  let sup_model =
    envoy.get("BOUGH_EXP_BASELINE_MODEL")
    |> result.unwrap("claude-haiku-4-5-20251001")
  let tasks = filtered_tasks()
  let trials = env_int("BOUGH_EXP_TRIALS", 1)
  let jsonl = open_jsonl()

  io.println(
    "Escalation sweep: tier1=qwen one-shot → tier2=relay(≤"
    <> int.to_string(escalate_relay_iters)
    <> ") → tier3="
    <> sup_model
    <> " on CHECK fail\nexecutor="
    <> exec_model
    <> ", reasoner="
    <> reasoner_model
    <> "\n"
    <> int.to_string(list.length(tasks))
    <> " tasks × "
    <> int.to_string(trials)
    <> " trial(s)\n→ "
    <> jsonl
    <> "\n",
  )

  let metrics =
    list.flat_map(tasks, fn(task) {
      range(1, trials)
      |> list.map(fn(trial) {
        let m =
          run_escalate_one(
            sup_key, sup_model, reasoner_url, reasoner_model, exec_url,
            exec_model, task, trial,
          )
        let _ = simplifile.append(jsonl, metric_json(m) <> "\n")
        io.println(line(m))
        m
      })
    })

  io.println("\n" <> difficulty_summary(metrics))
  io.println("\n" <> latency_report(metrics))
}

fn run_escalate_one(
  sup_key: String,
  sup_model: String,
  reasoner_url: String,
  reasoner_model: String,
  exec_url: String,
  exec_model: String,
  task: Task,
  trial: Int,
) -> Metric {
  let ws =
    home()
    <> "/.bough/experiments/ws/"
    <> task.name
    <> "__escalate__t"
    <> int.to_string(trial)
    <> "_"
    <> int.to_string(clock.now_ms())
  let _ = simplifile.create_directory_all(ws)
  write_fixtures(ws, task.fixtures)

  let t0 = clock.now_ms()
  // Tier 1: fast executor one-shot.
  let _ = executor_oneshot(exec_url, exec_model, ws, task)
  let #(solved, outcome, rounds, execs) = case
    check_passes(ws, task.ground_truth)
  {
    True -> #(True, "tier1", 1, 1)
    // Tier 2: one local relay round (the task prompt stands in for the
    // supervisor brief — we're timing the local worker tiers).
    False -> {
      let #(iters, execs, _) =
        relay_loop(
          reasoner_url, reasoner_model, exec_url, exec_model, ws, task,
          task.prompt, escalate_relay_iters, "", 1, 1,
        )
      case check_passes(ws, task.ground_truth) {
        True -> #(True, "tier2@" <> int.to_string(iters), 1 + iters, execs)
        // Tier 3: the local stack hit its ceiling — hand off to the frontier
        // supervisor on a clean copy of the workspace (its own full loop).
        False ->
          case sup_key {
            "" -> #(False, "unsolved", 1 + iters, execs)
            _ -> {
              write_fixtures(ws, task.fixtures)
              let cfg =
                engine.Config(
                  ..engine.default_config(),
                  provider: provider.Anthropic,
                  worker: None,
                  max_rounds: 8,
                  max_steps: 60,
                )
              let _ = engine.run(sup_key, sup_model, ws, cfg, [], task.prompt, [])
              let solved3 = check_passes(ws, task.ground_truth)
              let label = case solved3 {
                True -> "tier3"
                False -> "unsolved"
              }
              #(solved3, label, 2 + iters, execs)
            }
          }
      }
    }
  }
  let wall = clock.now_ms() - t0
  Metric(
    task: task.name,
    difficulty: task.difficulty,
    cell: "escalate",
    trial: trial,
    solved: solved,
    outcome: outcome,
    rounds: rounds,
    exec_steps: execs,
    worker_fixes: 0,
    model_check_committed: False,
    model_check_passed: False,
    context_tokens: 0,
    wall_ms: wall,
  )
}

/// One fast executor attempt with primitives. Returns nothing — the caller reads
/// the workspace via the ground-truth check.
fn executor_oneshot(url: String, model: String, ws: String, task: Task) -> Nil {
  let user =
    "Files in the workspace:\n"
    <> current_files(ws, task.fixtures)
    <> "\nTask: "
    <> task.prompt
  let _ = apply_executor(url, model, ws, user)
  Nil
}

/// Solved rate aggregated by difficulty tier (clearer than a 15-column matrix
/// for the widened suite).
fn difficulty_summary(metrics: List(Metric)) -> String {
  let diffs =
    metrics
    |> list.map(fn(m) { m.difficulty })
    |> list.unique
    |> list.sort(int.compare)
  let rows =
    list.map(diffs, fn(d) {
      let ms = list.filter(metrics, fn(m) { m.difficulty == d })
      "  d" <> int.to_string(d) <> ": " <> rate(ms) <> " solved"
    })
  "Solved by difficulty tier:\n" <> string.join(rows, "\n")
}

/// p50/p95 wall-clock and the tier mix — the numbers that say whether the
/// escalation makes daily latency acceptable.
fn latency_report(metrics: List(Metric)) -> String {
  let walls = list.sort(list.map(metrics, fn(m) { m.wall_ms }), int.compare)
  let tier1 = count_metrics(metrics, fn(m) { m.outcome == "tier1" })
  let tier2 =
    count_metrics(metrics, fn(m) { string.starts_with(m.outcome, "tier2") })
  let tier3 = count_metrics(metrics, fn(m) { m.outcome == "tier3" })
  let unsolved = count_metrics(metrics, fn(m) { m.outcome == "unsolved" })
  let n = list.length(metrics)
  "Latency (wall ms): p50="
  <> int.to_string(percentile(walls, 50))
  <> " p95="
  <> int.to_string(percentile(walls, 95))
  <> " max="
  <> int.to_string(percentile(walls, 100))
  <> "\nResolved at: tier1(fast)="
  <> int.to_string(tier1)
  <> "  tier2(relay)="
  <> int.to_string(tier2)
  <> "  tier3(frontier)="
  <> int.to_string(tier3)
  <> "  unsolved="
  <> int.to_string(unsolved)
  <> "  (of "
  <> int.to_string(n)
  <> ")"
}

/// Nearest-rank percentile of an already-sorted list (0 if empty).
fn percentile(sorted: List(Int), p: Int) -> Int {
  let n = list.length(sorted)
  case n {
    0 -> 0
    _ -> {
      let rank = { p * n + 99 } / 100
      let idx = int.min(int.max(rank, 1), n) - 1
      case list.drop(sorted, idx) {
        [v, ..] -> v
        [] -> 0
      }
    }
  }
}

// --- Relay mode: supervisor → reasoner-worker → executor-worker -----------

/// Three tiers: the supervisor (Anthropic) writes a brief; the REASONER worker
/// (VibeThinker — strong at verifiable reasoning, bad at tool-use) decides the
/// change but touches nothing; the EXECUTOR worker (qwen2.5-coder + write/edit
/// primitives — proven at applying edits) carries it out. Results flow back up
/// to the reasoner for revision. Tests whether splitting "reason" from "act"
/// across two small models beats the executor alone.
const relay_max_iters = 3

const reasoner_system = "You are a senior coding worker who REASONS about the fix but never touches files. Given the task and the current file contents, work out exactly what change is needed and why. Output a precise, concrete instruction for your executor: name the file and describe the exact edit (the precise old code and the precise new code, or the full new file). Do not output tool calls or shell — just the instruction."

fn relay_sweep() -> Nil {
  let ollama = "http://127.0.0.1:11434"
  let reasoner_url = envoy.get("BOUGH_EXP_REASONER_URL") |> result.unwrap(ollama)
  let reasoner_model =
    envoy.get("BOUGH_EXP_REASONER_MODEL")
    |> result.unwrap("hf.co/mradermacher/VibeThinker-3B-GGUF:Q4_K_M")
  let exec_url = envoy.get("BOUGH_EXP_WORKER_URL") |> result.unwrap(ollama)
  let exec_model =
    envoy.get("BOUGH_EXP_WORKER_MODEL") |> result.unwrap("qwen2.5-coder:3b")
  let sup_key = envoy.get("ANTHROPIC_API_KEY") |> result.unwrap("")
  let sup_model =
    envoy.get("BOUGH_EXP_BASELINE_MODEL")
    |> result.unwrap("claude-haiku-4-5-20251001")
  let tasks = filtered_tasks()
  let trials = env_int("BOUGH_EXP_TRIALS", 1)
  let jsonl = open_jsonl()

  io.println(
    "Relay sweep (3 tiers): supervisor="
    <> sup_model
    <> " → reasoner="
    <> reasoner_model
    <> " → executor="
    <> exec_model
    <> "\n"
    <> int.to_string(list.length(tasks))
    <> " tasks × "
    <> int.to_string(trials)
    <> " trial(s)\n→ "
    <> jsonl
    <> "\n",
  )

  let metrics =
    list.flat_map(tasks, fn(task) {
      range(1, trials)
      |> list.map(fn(trial) {
        let m =
          run_relay_one(
            sup_key, sup_model, reasoner_url, reasoner_model, exec_url,
            exec_model, task, trial,
          )
        let _ = simplifile.append(jsonl, metric_json(m) <> "\n")
        io.println(line(m))
        m
      })
    })

  io.println("\n" <> summary(metrics, ["relay-3tier"], tasks))
}

fn run_relay_one(
  sup_key: String,
  sup_model: String,
  reasoner_url: String,
  reasoner_model: String,
  exec_url: String,
  exec_model: String,
  task: Task,
  trial: Int,
) -> Metric {
  let ws =
    home()
    <> "/.bough/experiments/ws/"
    <> task.name
    <> "__relay__t"
    <> int.to_string(trial)
    <> "_"
    <> int.to_string(clock.now_ms())
  let _ = simplifile.create_directory_all(ws)
  write_fixtures(ws, task.fixtures)

  let brief = supervisor_brief(sup_key, sup_model, task)
  let t0 = clock.now_ms()
  let #(iters, execs, outcome) =
    relay_loop(
      reasoner_url, reasoner_model, exec_url, exec_model, ws, task, brief,
      relay_max_iters, "", 1, 0,
    )
  let wall = clock.now_ms() - t0
  let solved = check_passes(ws, task.ground_truth)
  Metric(
    task: task.name,
    difficulty: task.difficulty,
    cell: "relay-3tier",
    trial: trial,
    solved: solved,
    outcome: outcome,
    rounds: iters,
    exec_steps: execs,
    worker_fixes: 0,
    model_check_committed: False,
    model_check_passed: False,
    context_tokens: 0,
    wall_ms: wall,
  )
}

/// Tier 1: the supervisor turns the task into a delegation brief (plain prose).
/// Falls back to the bare task if no key / the call fails.
fn supervisor_brief(api_key: String, model: String, task: Task) -> String {
  case api_key {
    "" -> task.prompt
    _ -> {
      let sys =
        "You are the SUPERVISOR delegating to a reasoning worker. In 2-4 sentences, state the goal, the file(s) involved, and the exact success criteria. Reply in plain prose only — do NOT call any tool."
      let tool =
        json.preprocessed_array([
          json.object([
            #("name", json.string(tools.run_steps_name)),
            #("description", json.string(tools.run_steps_description())),
            #("input_schema", tools.run_steps_schema()),
          ]),
        ])
      case
        anthropic.complete(api_key, model, sys, [provider.user_text(task.prompt)], tool)
      {
        Ok(r) ->
          case string.trim(r.text) {
            "" -> task.prompt
            t -> t
          }
        Error(_) -> task.prompt
      }
    }
  }
}

fn relay_loop(
  reasoner_url: String,
  reasoner_model: String,
  exec_url: String,
  exec_model: String,
  ws: String,
  task: Task,
  brief: String,
  max_iters: Int,
  history: String,
  iter: Int,
  execs: Int,
) -> #(Int, Int, String) {
  case iter > max_iters {
    True -> #(iter - 1, execs, "exhausted")
    False -> {
      let r_user =
        "Task brief:\n"
        <> brief
        <> "\n\nCurrent files:\n"
        <> current_files(ws, task.fixtures)
        <> history
      // Reasoner: VibeThinker decoding (temp 1.0 / top_p 0.95), long budget.
      case
        worker.complete_with(
          reasoner_url,
          reasoner_model,
          reasoner_system,
          r_user,
          6000,
          option.Some(1.0),
          option.Some(0.95),
        )
      {
        Error(_) -> #(iter, execs, "reasoner_error")
        Ok(rtext) -> {
          let instruction = strip_think(rtext)
          let e_user =
            "Your lead's instruction:\n"
            <> instruction
            <> "\n\nCurrent files:\n"
            <> current_files(ws, task.fixtures)
          // Executor acts via the configured channel (monty code-mode default).
          let #(n, summary, out) =
            apply_executor(exec_url, exec_model, ws, e_user)
          case n {
            0 -> #(iter, execs, "no_ops")
            _ -> {
              let execs = execs + n
              case check_passes(ws, task.ground_truth) {
                True -> #(iter, execs, "solved@" <> int.to_string(iter))
                False -> {
                  let note =
                    "\n\n[attempt "
                    <> int.to_string(iter)
                    <> "] executor did: "
                    <> summary
                    <> "\nresult: "
                    <> digest.digest(out, 400)
                    <> "\nThe task is NOT yet complete — revise your instruction."
                  relay_loop(
                    reasoner_url, reasoner_model, exec_url, exec_model, ws, task,
                    brief, max_iters, history <> note, iter + 1, execs,
                  )
                }
              }
            }
          }
        }
      }
    }
  }
}

/// The current contents of the task's files (they change as the executor acts).
fn current_files(ws: String, fixtures: List(#(String, String))) -> String {
  fixtures
  |> list.map(fn(f) {
    let content = simplifile.read(ws <> "/" <> f.0) |> result.unwrap("(missing)")
    "=== " <> f.0 <> " ===\n" <> content
  })
  |> string.join("\n")
}

/// Drop a reasoning model's `<think>…</think>` block, keeping the answer after
/// it (VibeThinker emits long CoT before its instruction).
fn strip_think(text: String) -> String {
  case string.split_once(text, "</think>") {
    Ok(#(_, after)) -> string.trim(after)
    // No closed think block (e.g. ran out of tokens) — use the text as-is.
    Error(_) -> string.trim(text)
  }
}

// --- Executor action surface: monty code-mode vs. primitives --------------

/// bough's real action surface (SPEC §5.2): the executor writes ONE Python
/// program run in the monty sandbox, reaching the workspace only through the
/// host functions. Files it write()s run later under real python3, so classes
/// etc. are fine in those files even though the monty program itself is a subset.
const monty_system = "You are a coding worker in a workspace directory. Accomplish the task by writing ONE Python program (inside a single ```python code block) that calls these host functions and prints what matters:
- bash(cmd) -> str : run a shell command in the sandbox
- read(path) -> str : read a workspace file
- write(path, content) : create or overwrite a workspace file
- edit(path, old, new) : replace the single exact, unique occurrence of `old` with `new`
Inspect, change, and verify in that one program. It runs in a monty sandbox — a Python SUBSET: stdlib only (no third-party imports), and no class or match statements IN THIS PROGRAM. Files you write() may contain anything (classes included) since they run later under real python3 — so to deliver a class-based solution, pass its full source as a string to write(). Respond with ONLY the ```python block."

/// The executor's action channel, via BOUGH_EXP_EXEC. Default `primitives` (the
/// harness's fenced write/edit/sh blocks) — the surface small executors handle,
/// and the one that yields meaningful capability/latency numbers. `monty` routes
/// through the real code-mode sidecar; it's faithful to the SUPERVISOR's surface
/// but small executors collapse on its indirection (they can't write a program
/// that writes the file), so it's for studying that wall, not measuring workers.
fn exec_mode() -> String {
  envoy.get("BOUGH_EXP_EXEC") |> result.unwrap("primitives")
}

/// Run the executor on `user`, applying its actions to the workspace. Returns
/// #(n_actions, summary, output) — n_actions=0 means it produced nothing usable.
fn apply_executor(
  url: String,
  model: String,
  ws: String,
  user: String,
) -> #(Int, String, String) {
  case exec_mode() {
    "primitives" ->
      case
        worker.complete_with(url, model, prim_system, user, 4096, option.Some(0.2), option.None)
      {
        Error(_) -> #(0, "(executor error)", "")
        Ok(text) -> {
          let ops = parse_ops(text)
          #(list.length(ops), ops_summary(ops), apply_ops(ws, ops))
        }
      }
    // monty code-mode (default): one Python program through the sidecar.
    _ ->
      case
        worker.complete_with(url, model, monty_system, user, 4096, option.Some(0.2), option.None)
      {
        Error(_) -> #(0, "(executor error)", "")
        Ok(text) ->
          case extract_program(text) {
            None -> #(0, "(no program)", "")
            Some(prog) -> {
              let #(_exit, out) = monty_bridge.run_code(ws, prog, None)
              #(1, "monty program", out)
            }
          }
      }
  }
}

/// The first fenced code block's body — the monty program the executor emitted.
fn extract_program(text: String) -> Option(String) {
  case fenced_blocks(text) {
    [#(_, body), ..] -> Some(body)
    [] -> None
  }
}

// --- Worker primitive ops (write / edit / run) ----------------------------

type Op {
  OpWrite(path: String, content: String)
  OpEdit(path: String, search: String, replace: String)
  OpRun(cmd: String)
}

/// Parse the worker's fenced blocks into ops. Each block's info string after the
/// opening fence selects the op: `write <path>`, `edit <path>`, or `sh`.
fn parse_ops(text: String) -> List(Op) {
  fenced_blocks(text)
  |> list.filter_map(fn(block) {
    let #(info, body) = block
    let tokens = string.split(string.trim(info), " ")
    case tokens {
      ["write", path, ..] -> Ok(OpWrite(path, strip_trailing_nl(body)))
      ["edit", path, ..] ->
        case parse_search_replace(body) {
          Ok(#(s, r)) -> Ok(OpEdit(path, s, r))
          Error(_) -> Error(Nil)
        }
      ["sh", ..] | ["bash", ..] | ["shell", ..] -> Ok(OpRun(string.trim(body)))
      _ -> Error(Nil)
    }
  })
}

fn apply_ops(ws: String, ops: List(Op)) -> String {
  ops
  |> list.map(fn(op) { apply_op(ws, op) })
  |> string.join("\n")
}

fn apply_op(ws: String, op: Op) -> String {
  case op {
    OpWrite(path, content) -> {
      let dest = resolve(ws, path)
      let _ = simplifile.create_directory_all(parent_dir(dest))
      case simplifile.write(dest, content) {
        Ok(_) -> "wrote " <> path
        Error(e) -> "write failed: " <> string.inspect(e)
      }
    }
    OpEdit(path, search, replace) -> {
      let dest = resolve(ws, path)
      case simplifile.read(dest) {
        Error(e) -> "edit: cannot read " <> path <> ": " <> string.inspect(e)
        Ok(contents) ->
          case string.contains(contents, search) {
            False -> "edit: SEARCH not found in " <> path
            True -> {
              // Replace only the first occurrence.
              let fixed = case string.split_once(contents, search) {
                Ok(#(before, after)) -> before <> replace <> after
                Error(_) -> contents
              }
              case simplifile.write(dest, fixed) {
                Ok(_) -> "edited " <> path
                Error(e) -> "edit: write failed: " <> string.inspect(e)
              }
            }
          }
      }
    }
    OpRun(cmd) ->
      nono_bridge.run(nono.Profile(ws, [], True, False), ["sh", "-c", cmd])
  }
}

fn ops_summary(ops: List(Op)) -> String {
  ops
  |> list.map(fn(op) {
    case op {
      OpWrite(p, _) -> "write " <> p
      OpEdit(p, _, _) -> "edit " <> p
      OpRun(c) -> "sh " <> c
    }
  })
  |> string.join("; ")
}

/// Extract `#(info_string, body)` for every fenced ``` block in the text.
fn fenced_blocks(text: String) -> List(#(String, String)) {
  collect_fences(string.split(text, "\n"), None, [], [])
}

fn collect_fences(
  lines: List(String),
  open: Option(String),
  body_rev: List(String),
  acc: List(#(String, String)),
) -> List(#(String, String)) {
  case lines {
    [] -> list.reverse(acc)
    [line, ..rest] ->
      case string.starts_with(string.trim_start(line), "```") {
        False ->
          case open {
            Some(_) -> collect_fences(rest, open, [line, ..body_rev], acc)
            None -> collect_fences(rest, None, body_rev, acc)
          }
        True ->
          case open {
            // Closing fence: emit the block.
            Some(info) -> {
              let body = string.join(list.reverse(body_rev), "\n")
              collect_fences(rest, None, [], [#(info, body), ..acc])
            }
            // Opening fence: capture the info string after the backticks.
            None -> {
              let info =
                string.trim(line)
                |> string.replace("```", "")
              collect_fences(rest, Some(info), [], acc)
            }
          }
      }
  }
}

/// Split an `edit` block body into #(search, replace) around the SEARCH/REPLACE
/// conflict markers. Tolerant of how small models malform the block: the SEARCH
/// text may sit BELOW the `<<<<<<<` line (canonical) OR inlined ON it (a common
/// failure — e.g. `<<<<<<< old_code`); same for REPLACE on the `=======` line.
/// A state machine collects the block bodies and the marker-line trailers, then
/// prefers the block but falls back to the inline trailer when the block is empty.
fn parse_search_replace(body: String) -> Result(#(String, String), Nil) {
  let init = #("pre", [], [], "", "")
  let #(section, srev, rrev, sinl, rinl) =
    list.fold(string.split(body, "\n"), init, fn(acc, line) {
      let #(sec, sr, rr, si, ri) = acc
      let ts = string.trim_start(line)
      let kind = case string.starts_with(ts, "<<<<<<<") {
        True -> "<"
        False ->
          case string.starts_with(ts, "=======") {
            True -> "="
            False ->
              case string.starts_with(ts, ">>>>>>>") {
                True -> ">"
                False -> "p"
              }
          }
      }
      case kind {
        "<" -> #("search", sr, rr, after_marker(ts, "<"), ri)
        "=" -> #("replace", sr, rr, si, after_marker(ts, "="))
        ">" -> #("post", sr, rr, si, ri)
        _ ->
          case sec {
            "search" -> #(sec, [line, ..sr], rr, si, ri)
            "replace" -> #(sec, sr, [line, ..rr], si, ri)
            _ -> acc
          }
      }
    })
  // Must have crossed the separator (saw both `<<<<<<<` and `=======`).
  case section {
    "replace" | "post" -> {
      let sblock = strip_edges_nl(string.join(list.reverse(srev), "\n"))
      let rblock = strip_edges_nl(string.join(list.reverse(rrev), "\n"))
      let search = case string.trim(sblock) {
        "" -> sinl
        _ -> sblock
      }
      let replace = case string.trim(rblock) {
        "" -> rinl
        _ -> rblock
      }
      case search {
        "" -> Error(Nil)
        _ -> Ok(#(search, replace))
      }
    }
    _ -> Error(Nil)
  }
}

/// The text after a run of `ch` (the marker char) at the start of a line —
/// the inline trailer, e.g. `<<<<<<< foo` → `foo`. Empty for a bare marker.
fn after_marker(line: String, ch: String) -> String {
  string.trim(drop_leading(line, ch))
}

fn drop_leading(s: String, ch: String) -> String {
  case string.starts_with(s, ch) {
    True -> drop_leading(string.drop_start(s, 1), ch)
    False -> s
  }
}

fn strip_edges_nl(s: String) -> String {
  s
  |> trim_leading_nl
  |> strip_trailing_nl
}

fn trim_leading_nl(s: String) -> String {
  case string.starts_with(s, "\n") {
    True -> trim_leading_nl(string.drop_start(s, 1))
    False -> s
  }
}

fn strip_trailing_nl(s: String) -> String {
  case string.ends_with(s, "\n") {
    True -> strip_trailing_nl(string.drop_end(s, 1))
    False -> s
  }
}

fn resolve(ws: String, path: String) -> String {
  case string.starts_with(path, "/") {
    True -> path
    False -> ws <> "/" <> path
  }
}

fn parent_dir(path: String) -> String {
  case string.split(path, "/") {
    [_only] -> "."
    parts -> string.join(list.take(parts, list.length(parts) - 1), "/")
  }
}

/// Run one (cell, task, trial): fresh workspace, real engine loop with the cell's
/// supervisor, then OUR ground-truth check on the resulting files.
fn run_one(cell: Cell, task: Task, trial: Int) -> Metric {
  let ws =
    home()
    <> "/.bough/experiments/ws/"
    <> task.name
    <> "__"
    <> cell.label
    <> "__t"
    <> int.to_string(trial)
    <> "_"
    <> int.to_string(clock.now_ms())
  let _ = simplifile.create_directory_all(ws)
  write_fixtures(ws, task.fixtures)

  let config =
    engine.Config(
      ..engine.default_config(),
      provider: cell.provider,
      // Disable the nested fix-worker: this experiment measures the supervisor
      // model alone, so a same-model fix loop would muddy the signal.
      worker: None,
      max_rounds: cell.max_rounds,
      max_steps: cell.max_rounds * 8,
    )

  let t0 = clock.now_ms()
  let res =
    engine.run(cell.api_key, cell.model, ws, config, [], task.prompt, [])
  let wall = clock.now_ms() - t0

  let solved = check_passes(ws, task.ground_truth)
  case res {
    Error(e) ->
      Metric(
        task: task.name,
        difficulty: task.difficulty,
        cell: cell.label,
        trial: trial,
        solved: solved,
        outcome: "engine_error: " <> truncate(e, 60),
        rounds: 0,
        exec_steps: 0,
        worker_fixes: 0,
        model_check_committed: False,
        model_check_passed: False,
        context_tokens: 0,
        wall_ms: wall,
      )
    Ok(o) ->
      Metric(
        task: task.name,
        difficulty: task.difficulty,
        cell: cell.label,
        trial: trial,
        solved: solved,
        outcome: classify(o.steps),
        rounds: o.turns,
        exec_steps: count(o.steps, is_exec),
        worker_fixes: count(o.steps, is_worker),
        model_check_committed: list.any(o.steps, is_check),
        model_check_passed: last_check_ok(o.steps),
        context_tokens: o.context_tokens,
        wall_ms: wall,
      )
  }
}

/// Why the run stopped — the overload taxonomy. A capable supervisor reaches
/// `done`; an overloaded small model tends to exhaust rounds, loop, return empty
/// turns, or error. (`done` here is the model's claim; `solved` is the truth.)
fn classify(steps: List(Step)) -> String {
  let has = fn(needle) {
    list.any(steps, fn(s) {
      case s {
        StepText(t) -> string.contains(t, needle)
        _ -> False
      }
    })
  }
  case has("Round budget exhausted") {
    True -> "round_budget"
    False ->
      case has("Budget exhausted") {
        True -> "step_budget"
        False ->
          case has("empty turn") {
            True -> "empty_turn"
            False ->
              case has("refusal") {
                True -> "refusal"
                False ->
                  case has("⚠ error:") {
                    True -> "call_error"
                    False ->
                  case
                    list.any(steps, fn(s) {
                      case s {
                        StepReview(n) -> string.contains(n, "accepted")
                        _ -> False
                      }
                    })
                  {
                    True -> "done"
                    False -> "stopped"
                  }
                  }
              }
          }
      }
  }
}

// --- Cells from env -------------------------------------------------------

fn cells_from_env() -> List(Cell) {
  let worker_url =
    envoy.get("BOUGH_EXP_WORKER_URL")
    |> result.unwrap("http://127.0.0.1:8080")
  // The worker ladder: sweep model size × autonomy budget. Each (model, rounds)
  // is one cell, so the matrix shows the overload frontier moving with size.
  let models =
    envoy.get("BOUGH_EXP_WORKER_MODELS")
    |> result.unwrap("vibethinker-3b")
    |> string.split(",")
    |> list.map(string.trim)
    |> list.filter(fn(m) { m != "" })
  let rounds =
    envoy.get("BOUGH_EXP_ROUNDS")
    |> result.unwrap("1,3,6,12")
    |> string.split(",")
    |> list.filter_map(fn(s) { int.parse(string.trim(s)) })

  let worker_cells =
    list.flat_map(models, fn(model) {
      list.map(rounds, fn(r) {
        Cell(
          label: short(model) <> "-r" <> int.to_string(r),
          provider: provider.OpenAICompat(worker_url <> "/v1"),
          api_key: "",
          model: model,
          max_rounds: r,
        )
      })
    })

  case envoy.get("BOUGH_EXP_BASELINE"), envoy.get("ANTHROPIC_API_KEY") {
    Ok(_), Ok(key) -> {
      let m =
        envoy.get("BOUGH_EXP_BASELINE_MODEL")
        |> result.unwrap("claude-haiku-4-5")
      list.append(worker_cells, [
        Cell("baseline-r12", provider.Anthropic, key, m, 12),
      ])
    }
    _, _ -> worker_cells
  }
}

/// The base suite, chosen by BOUGH_EXP_SUITE: the default easy gradient,
/// `hard` (algorithms/stateful classes/multi-file/buried bugs — built to
/// separate executors once the easy suite saturates at 100%), or `all`.
fn base_suite() -> List(Task) {
  case envoy.get("BOUGH_EXP_SUITE") {
    Ok("hard") -> hard_suite()
    Ok("all") -> list.append(suite(), hard_suite())
    _ -> suite()
  }
}

fn filtered_tasks() -> List(Task) {
  let base = base_suite()
  case envoy.get("BOUGH_EXP_TASKS") {
    Error(_) -> base
    Ok(spec) -> {
      let names = spec |> string.split(",") |> list.map(string.trim)
      list.filter(base, fn(t) { list.contains(names, t.name) })
    }
  }
}

/// Ten harder tasks (difficulty 6-8): non-trivial algorithms (coin-change DP,
/// roman numerals, expression eval, topological cycle detection, spiral order),
/// stateful classes (LRU cache, sliding window), a multi-file coordinated fix,
/// and a buried bug in a larger file. All QA'd (fixture fails, correct passes),
/// int/string/bool/exception-valued.
fn hard_suite() -> List(Task) {
  [
    Task(
      name: "h_intervals",
      difficulty: 6,
      fixtures: [
        #(
          "intervals.py",
          "def merge(intervals):\n    res=[]\n    for s,e in intervals:\n        if res and s==res[-1][1]:\n            res[-1][1]=e\n        else:\n            res.append([s,e])\n    return res\n",
        ),
      ],
      prompt: "merge(intervals) in intervals.py should merge all overlapping intervals (each is [start,end]) and return them sorted by start, but it's buggy (it only joins exactly-touching, already-sorted intervals). Fix it. Example: merge([[1,3],[2,6],[8,10],[15,18]]) == [[1,6],[8,10],[15,18]].",
      ground_truth: "python3 -c \"from intervals import merge\nassert merge([[1,3],[2,6],[8,10],[15,18]])==[[1,6],[8,10],[15,18]]\nassert merge([[1,4],[4,5]])==[[1,5]]\nassert merge([[1,4],[2,3]])==[[1,4]]\nassert merge([[5,6],[1,2]])==[[1,2],[5,6]]\"",
    ),
    Task(
      name: "h_coins",
      difficulty: 7,
      fixtures: [#("coins.py", "def min_coins(coins, amount):\n    pass  # TODO\n")],
      prompt: "Implement min_coins(coins, amount) in coins.py: the minimum number of coins from `coins` that sum to `amount`, or -1 if impossible. Example: min_coins([1,2,5], 11) == 3.",
      ground_truth: "python3 -c \"from coins import min_coins\nassert min_coins([1,2,5],11)==3\nassert min_coins([2],3)==-1\nassert min_coins([1],0)==0\nassert min_coins([186,419,83,408],6249)==20\"",
    ),
    Task(
      name: "h_roman",
      difficulty: 7,
      fixtures: [#("roman.py", "def to_roman(n):\n    pass  # TODO\n")],
      prompt: "Implement to_roman(n) in roman.py for 1<=n<=3999, using standard subtractive notation (4=IV, 9=IX, 40=XL, 90=XC, 400=CD, 900=CM). Example: to_roman(1994) == 'MCMXCIV'.",
      ground_truth: "python3 -c \"from roman import to_roman\nassert to_roman(4)=='IV'\nassert to_roman(9)=='IX'\nassert to_roman(58)=='LVIII'\nassert to_roman(1994)=='MCMXCIV'\nassert to_roman(3888)=='MMMDCCCLXXXVIII'\"",
    ),
    Task(
      name: "h_lru",
      difficulty: 8,
      fixtures: [#("lru.py", "# Implement an LRU cache class.\n")],
      prompt: "In lru.py implement class LRUCache(capacity): get(key) returns the value or -1; put(key, value) inserts/updates; when over capacity, evict the least-recently-used entry. Both get and put count as a use.",
      ground_truth: "python3 -c \"from lru import LRUCache\nc=LRUCache(2)\nc.put(1,1); c.put(2,2)\nassert c.get(1)==1\nc.put(3,3)\nassert c.get(2)==-1\nc.put(4,4)\nassert c.get(1)==-1\nassert c.get(3)==3\nassert c.get(4)==4\"",
    ),
    Task(
      name: "h_eval",
      difficulty: 8,
      fixtures: [#("calc2.py", "def evaluate(expr):\n    pass  # TODO\n")],
      prompt: "Implement evaluate(expr) in calc2.py: evaluate a string arithmetic expression with + - * / and parentheses, standard precedence, integer division. Example: evaluate('2+3*4')==14, evaluate('(2+3)*4')==20.",
      ground_truth: "python3 -c \"from calc2 import evaluate\nassert evaluate('2+3*4')==14\nassert evaluate('(2+3)*4')==20\nassert evaluate('10-2-3')==5\nassert evaluate('2*(3+4)-5')==9\nassert evaluate('100/7')==14\"",
    ),
    Task(
      name: "h_shop",
      difficulty: 7,
      fixtures: [
        #("shop/__init__.py", ""),
        #("shop/cart.py", "def line_total(price, qty):\n    return price  # BUG: ignores qty\n"),
        #(
          "shop/order.py",
          "from shop.cart import line_total\n\ndef order_total(items):\n    # each item: {'price','qty','discount'}\n    return sum(line_total(i['price'], i['qty']) for i in items)\n",
        ),
      ],
      prompt: "Order totals are wrong: line items ignore quantity, and per-item discounts aren't applied at all. Each item is {'price','qty','discount'}; a line costs price*qty, and the order total subtracts each item's discount. Fix it (the logic spans shop/cart.py and shop/order.py).",
      ground_truth: "python3 -c \"from shop.order import order_total\nassert order_total([{'price':10,'qty':2,'discount':5}])==15\nassert order_total([{'price':3,'qty':3,'discount':0},{'price':5,'qty':1,'discount':2}])==12\"",
    ),
    Task(
      name: "h_grades",
      difficulty: 7,
      fixtures: [
        #(
          "grades.py",
          "# Grade utilities for a course gradebook.\n\ndef average(scores):\n    if not scores:\n        return 0\n    return sum(scores) // len(scores)\n\ndef letter_grade(score):\n    # Convert a 0-100 score to a letter grade.\n    if score > 90:\n        return \"A\"\n    elif score > 80:\n        return \"B\"\n    elif score > 70:\n        return \"C\"\n    elif score > 60:\n        return \"D\"\n    else:\n        return \"F\"\n\ndef passing(score):\n    return letter_grade(score) != \"F\"\n\ndef best(scores):\n    return max(scores) if scores else None\n",
        ),
      ],
      prompt: "letter_grade(score) in grades.py has an off-by-one at every boundary (90 should be 'A', 80 'B', 70 'C', 60 'D'). Fix only that function; leave the rest of the file unchanged.",
      ground_truth: "python3 -c \"from grades import letter_grade\nassert letter_grade(90)=='A'\nassert letter_grade(89)=='B'\nassert letter_grade(80)=='B'\nassert letter_grade(100)=='A'\nassert letter_grade(59)=='F'\nassert letter_grade(60)=='D'\"",
    ),
    Task(
      name: "h_schedule",
      difficulty: 8,
      fixtures: [#("schedule.py", "def can_finish(num_courses, prereqs):\n    pass  # TODO\n")],
      prompt: "Implement can_finish(num_courses, prereqs) in schedule.py: return True iff all courses can be finished. prereqs is a list of [a,b] meaning b must come before a; it's feasible iff the dependency graph has no cycle.",
      ground_truth: "python3 -c \"from schedule import can_finish\nassert can_finish(2,[[1,0]])==True\nassert can_finish(2,[[1,0],[0,1]])==False\nassert can_finish(4,[[1,0],[2,1],[3,2]])==True\nassert can_finish(3,[[0,1],[1,2],[2,0]])==False\"",
    ),
    Task(
      name: "h_window",
      difficulty: 7,
      fixtures: [
        #(
          "window.py",
          "class WindowSum:\n    def __init__(self, size):\n        self.size=size\n        self.vals=[]\n    def next(self, val):\n        self.vals.append(val)\n        return sum(self.vals)  # BUG: never drops old values\n",
        ),
      ],
      prompt: "WindowSum(size).next(val) in window.py should return the sum of only the last `size` values, but it sums every value ever seen. Fix it.",
      ground_truth: "python3 -c \"from window import WindowSum\nw=WindowSum(3)\nassert w.next(1)==1\nassert w.next(2)==3\nassert w.next(3)==6\nassert w.next(4)==9\nassert w.next(5)==12\"",
    ),
    Task(
      name: "h_spiral",
      difficulty: 8,
      fixtures: [#("spiral.py", "def spiral_order(matrix):\n    pass  # TODO\n")],
      prompt: "Implement spiral_order(matrix) in spiral.py: return the elements of the 2D list in clockwise spiral order starting top-left. Example: spiral_order([[1,2,3],[4,5,6],[7,8,9]]) == [1,2,3,6,9,8,7,4,5].",
      ground_truth: "python3 -c \"from spiral import spiral_order\nassert spiral_order([[1,2,3],[4,5,6],[7,8,9]])==[1,2,3,6,9,8,7,4,5]\nassert spiral_order([[1,2],[3,4]])==[1,2,4,3]\nassert spiral_order([[1]])==[1]\"",
    ),
  ]
}

// --- Workspace & grading --------------------------------------------------

fn write_fixtures(ws: String, fixtures: List(#(String, String))) -> Nil {
  list.each(fixtures, fn(f) {
    let path = ws <> "/" <> f.0
    let dir = case string.split(f.0, "/") {
      [_single] -> ws
      parts -> ws <> "/" <> string.join(list.take(parts, list.length(parts) - 1), "/")
    }
    let _ = simplifile.create_directory_all(dir)
    let _ = simplifile.write(path, f.1)
    Nil
  })
}

/// Run our ground-truth check unsandboxed in the finished workspace. Exit 0 =
/// solved. This is the measurement, not an agent action, so it needs no sandbox.
fn check_passes(ws: String, check: String) -> Bool {
  case shellout.command("sh", ["-c", check], ws, []) {
    Ok(_) -> True
    Error(_) -> False
  }
}

// --- Step inspection ------------------------------------------------------

fn is_exec(s: Step) -> Bool {
  case s {
    StepExec(_, _, _) -> True
    _ -> False
  }
}

fn is_worker(s: Step) -> Bool {
  case s {
    StepWorker(_, _) -> True
    _ -> False
  }
}

fn is_check(s: Step) -> Bool {
  case s {
    StepCheck(_, _) -> True
    _ -> False
  }
}

fn last_check_ok(steps: List(Step)) -> Bool {
  list.fold(steps, False, fn(acc, s) {
    case s {
      StepCheck(ok, _) -> ok
      _ -> acc
    }
  })
}

fn count(steps: List(Step), pred: fn(Step) -> Bool) -> Int {
  list.fold(steps, 0, fn(n, s) {
    case pred(s) {
      True -> n + 1
      False -> n
    }
  })
}

// --- Output ---------------------------------------------------------------

fn line(m: Metric) -> String {
  let mark = case m.solved {
    True -> "✓"
    False -> "✗"
  }
  mark
  <> " "
  <> pad(m.cell, 20)
  <> " "
  <> pad(m.task, 14)
  <> " "
  <> pad(m.outcome, 16)
  <> " rounds="
  <> int.to_string(m.rounds)
  <> " steps="
  <> int.to_string(m.exec_steps)
  <> " tok="
  <> int.to_string(m.context_tokens)
  <> " "
  <> int.to_string(m.wall_ms)
  <> "ms"
}

/// A solved-rate matrix: rows = cells/levels, columns = tasks (in difficulty
/// order), so the overload frontier reads as the diagonal where ✓ turns to ✗.
fn summary(metrics: List(Metric), labels: List(String), tasks: List(Task)) -> String {
  let sorted = list.sort(tasks, fn(a, b) { int.compare(a.difficulty, b.difficulty) })
  let header =
    pad("cell \\ task", 20)
    <> string.concat(list.map(sorted, fn(t) { pad("d" <> int.to_string(t.difficulty), 6) }))
  let rows =
    list.map(labels, fn(label) {
      pad(label, 20)
      <> string.concat(
        list.map(sorted, fn(t) {
          let cell_metrics =
            list.filter(metrics, fn(m) { m.cell == label && m.task == t.name })
          pad(rate(cell_metrics), 6)
        }),
      )
    })
  "Solved rate (independent ground-truth check):\n"
  <> header
  <> "\n"
  <> string.join(rows, "\n")
  <> "\n\nLegend: each cell = solved/total over trials. Overload frontier = where ✓→✗ as difficulty rises."
}

fn rate(metrics: List(Metric)) -> String {
  case metrics {
    [] -> "-"
    _ -> {
      let total = list.length(metrics)
      let solved = count_metrics(metrics, fn(m) { m.solved })
      int.to_string(solved) <> "/" <> int.to_string(total)
    }
  }
}

fn count_metrics(metrics: List(Metric), pred: fn(Metric) -> Bool) -> Int {
  list.fold(metrics, 0, fn(n, m) {
    case pred(m) {
      True -> n + 1
      False -> n
    }
  })
}

fn metric_json(m: Metric) -> String {
  json.to_string(
    json.object([
      #("task", json.string(m.task)),
      #("difficulty", json.int(m.difficulty)),
      #("cell", json.string(m.cell)),
      #("trial", json.int(m.trial)),
      #("solved", json.bool(m.solved)),
      #("outcome", json.string(m.outcome)),
      #("rounds", json.int(m.rounds)),
      #("exec_steps", json.int(m.exec_steps)),
      #("worker_fixes", json.int(m.worker_fixes)),
      #("model_check_committed", json.bool(m.model_check_committed)),
      #("model_check_passed", json.bool(m.model_check_passed)),
      #("context_tokens", json.int(m.context_tokens)),
      #("wall_ms", json.int(m.wall_ms)),
    ]),
  )
}

// --- Small helpers --------------------------------------------------------

/// A compact, label-safe model name: drop a registry path and the ":latest" tag.
fn short(model: String) -> String {
  let base = case string.split(model, "/") |> list.last {
    Ok(b) -> b
    Error(_) -> model
  }
  case string.ends_with(base, ":latest") {
    True -> string.replace(base, ":latest", "")
    False -> base
  }
}

fn home() -> String {
  envoy.get("HOME") |> result.unwrap("/tmp")
}

/// Inclusive 1..n (n<1 → empty).
fn range(from: Int, to: Int) -> List(Int) {
  case from > to {
    True -> []
    False -> [from, ..range(from + 1, to)]
  }
}

fn env_int(name: String, default: Int) -> Int {
  case envoy.get(name) {
    Ok(v) -> int.parse(v) |> result.unwrap(default)
    Error(_) -> default
  }
}

fn pad(s: String, n: Int) -> String {
  case string.length(s) >= n {
    True -> s <> " "
    False -> s <> string.repeat(" ", n - string.length(s))
  }
}

fn truncate(s: String, n: Int) -> String {
  case string.length(s) > n {
    True -> string.slice(s, 0, n) <> "…"
    False -> s
  }
}
