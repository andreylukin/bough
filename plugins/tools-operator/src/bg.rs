//! Invariant: a background job is owned by this row. `bg_max` bounds how many can be live, and
//! disposing the row kills every one of them — unwind leaves no orphan process.
//!
//! The output is TEE'd to `bg_log_dir/<id>.log` by handing the child that file as its stdout and
//! stderr, so the log is a fact of the filesystem and not of a reader task: killing the job, or
//! dropping the whole registry, leaves a log that is still readable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_plugin_tools::{FailureClass, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};
use parking_lot::Mutex;

use crate::OperatorConfig;

bough_util::brand_id!(
    /// One background job.
    pub struct BgId;
);

/// A live job, as `bg.output` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: BgId,
    pub name: String,
    pub cmd: String,
    pub pid: Option<u32>,
    pub exit: Option<i32>,
    /// Where the tee'd output lives: `bg_log_dir/<id>.log`.
    pub log: PathBuf,
}

struct Entry {
    job: Job,
    /// `None` once the job has been reaped or killed.
    child: Option<tokio::process::Child>,
}

/// The row's registry of background jobs.
///
/// It is a plain object with no `ctx`: `lib.rs` owns exactly one of these per row and defers
/// [`BgJobs::kill_all`] as the row's inverse, which is what makes "unwind leaves no orphan" a
/// property of the effect stack rather than of a destructor nobody runs.
pub struct BgJobs {
    cfg: Arc<OperatorConfig>,
    root: PathBuf,
    jobs: Mutex<BTreeMap<BgId, Entry>>,
    next: std::sync::atomic::AtomicU64,
}

fn err(kind: FailureClass, message: impl Into<String>) -> ToolFailure {
    ToolFailure {
        kind,
        message: message.into(),
    }
}

impl BgJobs {
    /// `root` is the pinned workspace root: every job starts there, exactly as `bash` does.
    pub fn new(cfg: Arc<OperatorConfig>, root: PathBuf) -> Arc<BgJobs> {
        Arc::new(BgJobs {
            cfg,
            root,
            jobs: Mutex::new(BTreeMap::new()),
            next: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// How many jobs have not exited yet. This is what `bg_max` bounds.
    pub fn live(&self) -> usize {
        self.jobs
            .lock()
            .values()
            .filter(|e| e.child.is_some())
            .count()
    }

    /// Every job this registry has ever started, oldest id first.
    pub fn all(&self) -> Vec<Job> {
        self.jobs.lock().values().map(|e| e.job.clone()).collect()
    }

    /// Start one detached child, its stdout AND stderr pointed at `bg_log_dir/<id>.log`.
    pub fn start(&self, name: &str, cmd: &str) -> Result<Job, ToolFailure> {
        // The bound is on LIVE jobs: a finished job's log stays readable and costs nothing, so
        // counting it would make `bg_max` a lifetime quota rather than a concurrency bound.
        if self.live() >= self.cfg.bg_max {
            return Err(err(
                FailureClass::Blocked,
                format!(
                    "{} background jobs are already running (bg_max); kill one before starting \
                     another",
                    self.cfg.bg_max
                ),
            ));
        }
        let n = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id = BgId::new(format!("bg{n}"));
        std::fs::create_dir_all(&self.cfg.bg_log_dir).map_err(|e| {
            err(
                FailureClass::Error,
                format!(
                    "could not create the background log directory `{}`: {e}",
                    self.cfg.bg_log_dir.display()
                ),
            )
        })?;
        let log = self.cfg.bg_log_dir.join(format!("{id}.log"));
        let out = std::fs::File::create(&log).map_err(|e| {
            err(
                FailureClass::Error,
                format!("could not create `{}`: {e}", log.display()),
            )
        })?;
        let errf = out.try_clone().map_err(|e| {
            err(
                FailureClass::Error,
                format!("could not duplicate the log handle: {e}"),
            )
        })?;
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(&self.root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(errf))
            // NOT `kill_on_drop`: a background job outlives the call that started it. Its life is
            // bounded by `kill_all`, which the row defers as its inverse.
            .spawn()
            .map_err(|e| err(FailureClass::Error, format!("could not start `sh`: {e}")))?;
        let job = Job {
            id: id.clone(),
            name: name.to_string(),
            cmd: cmd.to_string(),
            pid: child.id(),
            exit: None,
            log,
        };
        self.jobs.lock().insert(
            id,
            Entry {
                job: job.clone(),
                child: Some(child),
            },
        );
        Ok(job)
    }

    /// The job's status and everything written to its log so far.
    ///
    /// Reaping happens here: a child that has exited is waited on, so the process table never
    /// keeps a zombie for a job whose output the model already read.
    pub fn output(&self, id: &BgId) -> Result<(Job, String), ToolFailure> {
        let job = {
            let mut jobs = self.jobs.lock();
            let entry = jobs
                .get_mut(id)
                .ok_or_else(|| err(FailureClass::NotFound, format!("no background job `{id}`")))?;
            if let Some(child) = entry.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        entry.job.exit = Some(status.code().unwrap_or(-1));
                        entry.child = None;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        return Err(err(
                            FailureClass::Error,
                            format!("could not poll job `{id}`: {e}"),
                        ))
                    }
                }
            }
            entry.job.clone()
        };
        // Read OUTSIDE the lock: the log can be large and nothing about it needs the registry.
        let text = std::fs::read_to_string(&job.log).unwrap_or_default();
        Ok((job, text))
    }

    /// Kill one job. A job that already exited is not an error: the answer is its status.
    pub fn kill(&self, id: &BgId) -> Result<Job, ToolFailure> {
        let mut jobs = self.jobs.lock();
        let entry = jobs
            .get_mut(id)
            .ok_or_else(|| err(FailureClass::NotFound, format!("no background job `{id}`")))?;
        if let Some(mut child) = entry.child.take() {
            let _ = child.start_kill();
            entry.job.exit = Some(-1);
        }
        Ok(entry.job.clone())
    }

    /// Kill every live job. The row's inverse; safe to call twice.
    pub fn kill_all(&self) {
        let mut jobs = self.jobs.lock();
        for entry in jobs.values_mut() {
            if let Some(mut child) = entry.child.take() {
                let _ = child.start_kill();
                entry.job.exit = Some(-1);
            }
        }
    }
}

/// One three-op tool — `{op: "start"|"output"|"kill"}` — sugared in JS as `bg(name, cmd)` /
/// `bg.output(id)` / `bg.kill(id)`.
pub struct Bg {
    pub cfg: Arc<OperatorConfig>,
    pub jobs: Arc<BgJobs>,
}

fn arg_str(call: &ToolCall, key: &str) -> Result<String, ToolFailure> {
    call.args
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            err(
                FailureClass::Error,
                format!("`{key}` is required and must be a string"),
            )
        })
}

/// One line of status, the same for all three ops.
fn line(job: &Job) -> String {
    match job.exit {
        None => format!(
            "[{} {}] running (pid {})\n  log: {}",
            job.id,
            job.name,
            job.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
            job.log.display()
        ),
        Some(code) => format!(
            "[{} {}] exited {}\n  log: {}",
            job.id,
            job.name,
            code,
            job.log.display()
        ),
    }
}

fn value(job: &Job) -> serde_json::Value {
    serde_json::json!({
        "id": job.id,
        "name": job.name,
        "pid": job.pid,
        "exit": job.exit,
        "log": job.log.display().to_string(),
        "running": job.exit.is_none(),
    })
}

#[async_trait::async_trait]
impl Tool for Bg {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let op = arg_str(&call, "op")?;
        match op.as_str() {
            "start" => {
                let name = arg_str(&call, "name")?;
                let cmd = arg_str(&call, "cmd")?;
                let job = self.jobs.start(&name, &cmd)?;
                Ok(ToolOutcome {
                    content: line(&job),
                    value: Some(value(&job)),
                    ..Default::default()
                })
            }
            "output" => {
                let id = BgId::new(arg_str(&call, "id")?);
                let (job, text) = self.jobs.output(&id)?;
                let text = tail_bytes(&text, self.cfg.max_view_bytes);
                Ok(ToolOutcome {
                    content: format!("{}\n{text}", line(&job)),
                    value: Some(value(&job)),
                    ..Default::default()
                })
            }
            "kill" => {
                let id = BgId::new(arg_str(&call, "id")?);
                let job = self.jobs.kill(&id)?;
                Ok(ToolOutcome {
                    content: line(&job),
                    value: Some(value(&job)),
                    ..Default::default()
                })
            }
            other => Err(err(
                FailureClass::Error,
                format!("`op` must be one of start|output|kill, not `{other}`"),
            )),
        }
    }
}

/// Keep the LAST `max` bytes: a background job's interesting output is its most recent.
pub fn tail_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    // The EARLIEST boundary whose suffix still fits: anything later throws away output that was
    // asked for.
    let start = text
        .char_indices()
        .map(|(i, _)| i)
        .find(|i| text.len() - i <= max)
        .unwrap_or(0);
    format!("[{} earlier bytes elided]\n{}", start, &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_keeps_the_end_and_says_what_it_dropped() {
        let long = "abcdefghij".repeat(10);
        let out = tail_bytes(&long, 20);
        assert!(out.ends_with(&long[long.len() - 20..]), "{out}");
        assert!(out.contains("earlier bytes elided"), "{out}");
        assert_eq!(tail_bytes("short", 20), "short");
    }
}
