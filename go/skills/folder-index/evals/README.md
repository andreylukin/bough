# folder-index evals

An eval file is a YAML list of `{q, expect_path}` entries (bench.sh reads them with a regex: only single-line `- q:` / `expect_path:` pairs, no flow style or multi-line values): a question about the
folder and a path (relative to the folder) that a good answer cites. See
`example.yml`.

Run against a folder that already has `.index/` (built with
`/folder-index build <dir>`):

```sh
~/.bough/skills/folder-index/bench.sh <dir> evals/example.yml llm-anthropic/claude-sonnet-5
```

Each question runs twice, as fresh headless sessions: `index` (told to use
`<dir>/.index` via `/folder-index query`) and `baseline` (no index). The TSV
columns are `q`, `arm`, `hit` (1 if `expect_path` appears in the reply),
`seconds`, and `input_tokens`/`output_tokens` from the `[usage]` line printed
after `done`. Set `BOUGH_BIN` to test a local build.
