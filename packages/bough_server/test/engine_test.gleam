//// Tests for the engine's worker-op parser (the delegate executor surface).

import bough_server/engine.{WEdit, WRun, WWrite}

pub fn parse_write_block_test() {
  let text =
    "Here is the fix:\n```write math.py\ndef f(n):\n    return 1\n```\n"
  assert engine.parse_worker_ops(text) == [WWrite("math.py", "def f(n):\n    return 1")]
}

pub fn parse_edit_block_test() {
  let text =
    "```edit app.py\n<<<<<<< SEARCH\nreturn 0\n=======\nreturn 1\n>>>>>>> REPLACE\n```"
  assert engine.parse_worker_ops(text) == [WEdit("app.py", "return 0", "return 1")]
}

pub fn parse_sh_block_test() {
  assert engine.parse_worker_ops("```sh\nls -la && grep foo x.py\n```")
    == [WRun("ls -la && grep foo x.py")]
}

pub fn parse_multiple_blocks_in_order_test() {
  let text = "```write a.txt\nhi\n```\nthen\n```sh\ncat a.txt\n```"
  assert engine.parse_worker_ops(text) == [WWrite("a.txt", "hi"), WRun("cat a.txt")]
}

pub fn untagged_or_language_block_is_dropped_test() {
  // A generic ```python block (no write/edit/sh tag) carries no actionable op:
  // the small worker is told to tag its blocks, and an untagged block is more
  // likely prose/snippet than a full file — dropping it is the safe default.
  assert engine.parse_worker_ops("```python\nprint(1)\n```") == []
}

pub fn no_fences_yields_no_ops_test() {
  assert engine.parse_worker_ops("I cannot do this.") == []
}
