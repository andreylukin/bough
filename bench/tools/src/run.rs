//! Invariant: the two arms differ by ONE row — the consumer — and by nothing else. The same bank,
//! the same fixture repo, the same profile stack, the same model, the same transcripts' shape.
//!
//! The runner drives the RELEASE BINARY as a subprocess (`bough exec`), deliberately: the bench
//! must not be coupled to this workspace's compilation of the plugins under test, and what it
//! measures — steps appended, files on disk, journal rows, `usage/round` — is all durable state a
//! second process can read. `crates/bough/tests/exec_headless.rs` is the precedent.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bank::Task;
use crate::report::{price_round, Money, Price, Report, Row};

/// Which surface an arm measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arm {
    /// `tools-baseline` + `tools-operator`, typed calls.
    Typed,
    /// One `run(program)` over the sandbox.
    Codemode,
}

impl Arm {
    pub const BOTH: [Arm; 2] = [Arm::Typed, Arm::Codemode];

    pub fn label(&self) -> &'static str {
        match self {
            Arm::Typed => "typed",
            Arm::Codemode => "codemode",
        }
    }

    /// The patch file that configures this arm (bench-only config; never the row selection).
    pub fn patch(&self) -> &'static str {
        match self {
            Arm::Typed => "arms/typed.yml",
            Arm::Codemode => "arms/codemode.yml",
        }
    }

    /// The profile `bough exec` runs under. It forces `headless`
    /// (`crates/bough/src/exec.rs::force_profile`), so BOTH arms say `headless` and the codemode
    /// arm reaches its consumer through [`Arm::profile_source`] instead.
    pub fn profile(&self) -> &'static str {
        "headless"
    }

    /// The SHIPPED profile document this arm's row list comes from, or `None` for the arm that is
    /// the shipped `headless` tree already — which, since 2026-08-28, is the CODE-MODE arm: code
    /// mode is the default consumer and the typed arm is the one that names a document.
    ///
    /// A `--patch` layer configures rows; it never creates them (`config::patch`, §0.5 — a patch
    /// naming an uncreated row is a warning, not a row). So the three codemode rows can only come
    /// from a bundle, and the bundle list comes from a profile document. The runner therefore
    /// builds a scratch `--root` that is the repo's `bundles/` and `profiles/` verbatim, with
    /// `profiles/headless.yml` replaced by THIS document renamed. `--root` is searched before
    /// `$BOUGH_HOME` (`bough::profile::search_roots`), so the home is not an override path here.
    /// The bench measures the bundle list that SHIPS: only the file name changes.
    pub fn profile_source(&self) -> Option<&'static str> {
        match self {
            Arm::Typed => Some("profiles/typed.yml"),
            Arm::Codemode => None,
        }
    }

    /// Where this arm's recorded transcripts live, relative to `bench/tools`.
    pub fn fixtures(&self) -> &'static str {
        match self {
            Arm::Typed => "fixtures/typed",
            Arm::Codemode => "fixtures/codemode",
        }
    }
}

/// A throwaway directory, removed on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "bough-bench-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("a scratch dir");
        Scratch(p)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The repo root, from this crate's manifest.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root exists")
}

/// The binary under test: `$BOUGH_BIN`, else `target/release/bough`, else `target/debug/bough`.
pub fn bough_bin() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("BOUGH_BIN") {
        let p = PathBuf::from(p);
        anyhow::ensure!(p.exists(), "$BOUGH_BIN `{}` does not exist", p.display());
        return Ok(p);
    }
    for candidate in ["target/release/bough", "target/debug/bough"] {
        let p = repo_root().join(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("no bough binary: build one (`cargo build --release`) or set $BOUGH_BIN")
}

/// The price table the $ column is arithmetic over, read from the SHIPPED bundle so the bench and
/// the product cannot disagree about what a token costs.
pub fn price_table() -> anyhow::Result<std::collections::BTreeMap<String, Price>> {
    let text = std::fs::read_to_string(repo_root().join("bundles/bough-base.yml"))?;
    let rows: serde_yaml::Value = serde_yaml::from_str(&text)?;
    let prices = rows
        .as_sequence()
        .and_then(|rows| {
            rows.iter()
                .find(|r| r.get("id").and_then(|i| i.as_str()) == Some("model.policy"))
        })
        .and_then(|r| r.get("config"))
        .and_then(|c| c.get("prices"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("bough-base.yml has no `model.policy.prices`"))?;
    Ok(serde_yaml::from_value(prices)?)
}

/// What one `bough exec` produced, as durable state.
#[derive(Clone, Debug, Default)]
pub struct Observed {
    pub steps: u32,
    pub kinds: std::collections::BTreeMap<String, usize>,
    pub journal_kinds: std::collections::BTreeMap<String, usize>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: Option<String>,
}

/// Read the run's ledger. Read-only: the bench never writes to a tree it measures.
pub fn observe(ledger_db: &Path) -> anyhow::Result<Observed> {
    let mut o = Observed::default();
    if !ledger_db.exists() {
        return Ok(o);
    }
    let db = rusqlite::Connection::open_with_flags(
        ledger_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    {
        let mut q = db.prepare("SELECT type, body FROM steps ORDER BY traj_id, seq")?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let kind: String = r.get(0)?;
            let body: String = r.get(1)?;
            o.steps += 1;
            *o.kinds.entry(kind.clone()).or_default() += 1;
            if kind == bough_plugin_llm::USAGE_ROUND {
                if let Ok(u) = serde_json::from_str::<bough_plugin_llm::UsageRound>(&body) {
                    o.input_tokens += u.input_tokens.max(0) as u64;
                    o.output_tokens += u.output_tokens.max(0) as u64;
                    o.model.get_or_insert(u.model);
                }
            }
        }
    }
    let mut q = db.prepare("SELECT kind FROM actions")?;
    let mut rows = q.query([])?;
    while let Some(r) = rows.next()? {
        *o.journal_kinds.entry(r.get::<_, String>(0)?).or_default() += 1;
    }
    Ok(o)
}

/// Score a task's data predicates against the work tree and the observed ledger.
///
/// Returns `Ok(())` or the FIRST predicate that did not hold, spelled out.
pub fn score(task: &Task, work: &Path, o: &Observed) -> Result<(), String> {
    use crate::bank::Pass;
    for p in &task.pass {
        match p {
            Pass::FileEquals { path, text } => {
                let got =
                    std::fs::read_to_string(work.join(path)).map_err(|e| format!("{path}: {e}"))?;
                if got.trim_end() != text.trim_end() {
                    return Err(format!("{path} is not the expected text"));
                }
            }
            Pass::FileContains { path, needle } => {
                let got =
                    std::fs::read_to_string(work.join(path)).map_err(|e| format!("{path}: {e}"))?;
                if !got.contains(needle.as_str()) {
                    return Err(format!("{path} does not contain `{needle}`"));
                }
            }
            Pass::FileAbsent { path } => {
                if work.join(path).exists() {
                    return Err(format!("{path} exists and must not"));
                }
            }
            Pass::StepAppended { kind, count } => {
                let got = o.kinds.get(kind).copied().unwrap_or(0);
                if got < *count {
                    return Err(format!("{got} `{kind}` steps, wanted at least {count}"));
                }
            }
            Pass::JournalRow { kind } => {
                if o.journal_kinds.get(kind).copied().unwrap_or(0) == 0 {
                    return Err(format!("no `{kind}` row in the actions journal"));
                }
            }
        }
    }
    Ok(())
}

/// Boots the headless profile once per (task, arm) and scores the result.
pub struct Runner {
    /// `false` ⇒ `llm-replay` against the recorded transcripts; `true` (BOUGH_LIVE=1) ⇒ the
    /// profile's own `llm-anthropic` on haiku.
    pub live: bool,
    pub bin: PathBuf,
    pub bench_dir: PathBuf,
    pub prices: std::collections::BTreeMap<String, Price>,
}

impl Runner {
    pub fn new(live: bool) -> anyhow::Result<Runner> {
        Ok(Runner {
            live,
            bin: bough_bin()?,
            bench_dir: crate::bank::bench_dir(),
            prices: price_table()?,
        })
    }

    fn mode(&self) -> &'static str {
        if self.live {
            "live haiku"
        } else {
            "replay"
        }
    }

    /// The fixture repo, copied fresh so a task's edits never reach the next task.
    fn lay_out_work(&self, into: &Path) -> anyhow::Result<()> {
        copy_tree(&self.bench_dir.join("fixtures/repo"), into)
    }

    /// The `--root` this arm boots from: the repo itself for the typed arm, and for the codemode
    /// arm a scratch copy of the repo's `bundles/` and `profiles/` in which `headless.yml` IS
    /// `profiles/codemode.yml` (see [`Arm::profile_source`]). Only the document's name changes.
    fn lay_out_root(&self, home: &Path, arm: Arm) -> anyhow::Result<PathBuf> {
        // BOTH arms boot from a private copy, not from the checkout. Two reasons, and the second
        // is why the typed arm gets one too: the arms must differ in the consumer and in nothing
        // else (including "one of them reads a directory that can change under it"), and a bench
        // that reads the live `bundles/` scores whatever someone happened to be editing.
        let root = home.join("root");
        copy_tree(&repo_root().join("bundles"), &root.join("bundles"))?;
        copy_tree(&repo_root().join("profiles"), &root.join("profiles"))?;
        let Some(rel) = arm.profile_source() else {
            return Ok(root);
        };

        let src = repo_root().join(rel);
        let text = std::fs::read_to_string(&src)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", src.display()))?;
        let mut doc: serde_yaml::Value = serde_yaml::from_str(&text)?;
        doc.as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("{} is not a mapping", src.display()))?
            .insert(
                serde_yaml::Value::from("name"),
                serde_yaml::Value::from(arm.profile()),
            );
        std::fs::write(
            root.join("profiles").join(format!("{}.yml", arm.profile())),
            serde_yaml::to_string(&doc)?,
        )?;
        Ok(root)
    }

    /// Run one task under one arm and score it against its data predicates.
    ///
    /// A run that dies in the kernel's boot race (`docs/codemode-merge-notes.md` §11: a required
    /// service read racing the row that provides it, so `exec` comes up with no agent factory)
    /// measured NOTHING — no round was issued, no step was written, so its `$` is unknown rather
    /// than zero, and one such row voids the whole arm's `$` column. It is not a property of the
    /// surface under comparison, so it is retried rather than scored. Bounded, and only on that
    /// exact signature: any other non-zero exit is a real failure and is reported as one.
    pub fn run_one(&self, task: &Task, arm: Arm) -> anyhow::Result<Row> {
        let mut last = self.attempt(task, arm)?;
        for _ in 0..BOOT_RACE_RETRIES {
            if !is_boot_race(&last) {
                break;
            }
            last = self.attempt(task, arm)?;
        }
        Ok(last)
    }

    fn attempt(&self, task: &Task, arm: Arm) -> anyhow::Result<Row> {
        let home = Scratch::new("home");
        let work = Scratch::new("work");
        self.lay_out_work(work.path())?;

        let root = self.lay_out_root(home.path(), arm)?;
        let shim = gh_shim(home.path())?;
        let path = match std::env::var("PATH") {
            Ok(p) => format!("{}:{p}", shim.display()),
            Err(_) => shim.display().to_string(),
        };

        let mut cmd = Command::new(&self.bin);
        cmd.current_dir(work.path())
            .env("BOUGH_HOME", home.path())
            // AND `$HOME`: the shipped `old-feed` row defaults its db paths against `$HOME`, not
            // `$BOUGH_HOME`, so a bench left as-is would read the developer's real `~/.bough`
            // (`crates/bough/tests/support/mod.rs` says the same about the integration tests).
            .env("HOME", home.path())
            .env("BOUGH_BENCH_WORK", work.path())
            .env("PATH", path)
            .arg("--root")
            .arg(&root)
            .arg("--profile")
            .arg(arm.profile())
            .arg("--patch")
            .arg(self.bench_dir.join(arm.patch()));
        if !self.live {
            let fixture = self
                .bench_dir
                .join(arm.fixtures())
                .join(format!("{}.yml", task.id));
            anyhow::ensure!(
                fixture.exists(),
                "no recorded transcript for task `{}` under {} ({})",
                task.id,
                arm.label(),
                fixture.display()
            );
            cmd.arg("--patch").arg(fixture);
        }
        cmd.arg("exec").arg(&task.prompt);

        let out = cmd.output()?;
        let observed = observe(&home.path().join("ledger.db"))?;
        let mut note = None;
        let mut passed = out.status.success();
        if !passed {
            note = Some(format!(
                "exec exited {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .next_back()
                    .unwrap_or_default()
            ));
        } else if let Err(why) = score(task, work.path(), &observed) {
            passed = false;
            note = Some(why);
        }

        let price = observed.model.as_deref().and_then(|m| self.prices.get(m));
        Ok(Row {
            task: task.id.clone(),
            arm,
            passed,
            steps: observed.steps,
            input_tokens: observed.input_tokens,
            output_tokens: observed.output_tokens,
            cents: price_round(observed.input_tokens, observed.output_tokens, price),
            note,
        })
    }

    /// The whole bank, both arms, in a stable order.
    pub fn run_bank(&self, tasks: &[Task]) -> anyhow::Result<Report> {
        let mut rows = Vec::with_capacity(tasks.len() * 2);
        for arm in Arm::BOTH {
            for task in tasks {
                rows.push(self.run_one(task, arm)?);
            }
        }
        Ok(Report {
            rows,
            mode: self.mode().to_string(),
        })
    }
}

/// How many times a task killed by the kernel boot race (§11) is re-run before it is reported.
const BOOT_RACE_RETRIES: usize = 3;

/// The signature of merge-note §11 in a scored row: the process failed to boot, so it wrote no
/// step at all. Both halves are required — a run that produced steps and then exited non-zero is
/// a genuine failure of the task, and re-running it would launder a real result.
fn is_boot_race(row: &Row) -> bool {
    row.steps == 0
        && row
            .note
            .as_deref()
            .is_some_and(|n| n.starts_with("exec exited") && n.contains("no agent factory is set"))
}

/// The recording `gh`: FIRST on the run's PATH, so an `act` task cannot reach the real one.
///
/// It records its argv under the run's `$BOUGH_HOME` and exits 0 with a plausible PR url. Today
/// the `actions` seam has no `gh` Provider at all (the executor is Phase 8), so nothing invokes
/// it — the shim is the guard that keeps that true as the executor lands, not a fixture the bench
/// depends on. `docs/codemode-merge-notes.md` says so in one line.
pub fn gh_shim(home: &Path) -> anyhow::Result<PathBuf> {
    let dir = home.join("shim");
    std::fs::create_dir_all(&dir)?;
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$BOUGH_HOME/gh-calls.log\"\n         echo https://github.com/andreylukin/bough/pull/1\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(dir)
}

/// Recursive copy. `std::fs` has no such thing and the fixture repo is a dozen small files.
pub fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)
        .map_err(|e| anyhow::anyhow!("reading the fixture repo {}: {e}", from.display()))?
    {
        let entry = entry?;
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dst)?;
        } else {
            std::fs::copy(entry.path(), dst)?;
        }
    }
    Ok(())
}

/// Money helper for callers that already have a price and want the row's total.
pub fn total(rows: &[Row]) -> Option<Money> {
    rows.iter()
        .try_fold(Money(0), |a, r| r.cents.map(|c| a + c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(steps: u32, note: Option<&str>) -> Row {
        Row {
            task: "t".into(),
            arm: Arm::Typed,
            passed: false,
            steps,
            input_tokens: 0,
            output_tokens: 0,
            cents: None,
            note: note.map(str::to_string),
        }
    }

    #[test]
    fn a_run_that_died_before_the_agent_factory_mounted_is_a_boot_race() {
        assert!(is_boot_race(&row(
            0,
            Some("exec exited 1: row `exec`: no agent factory is set; mount an `agent-loop` row")
        )));
    }

    #[test]
    fn a_task_that_produced_steps_is_never_retried_even_with_that_message() {
        assert!(!is_boot_race(&row(
            12,
            Some("exec exited 1: row `exec`: no agent factory is set; mount an `agent-loop` row")
        )));
    }

    #[test]
    fn an_ordinary_failure_is_reported_not_retried() {
        assert!(!is_boot_race(&row(0, Some("exec exited 1: boom"))));
        assert!(!is_boot_race(&row(
            0,
            Some("src/a.txt is not the expected text")
        )));
        assert!(!is_boot_race(&row(0, None)));
    }
}
