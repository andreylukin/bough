//! Invariant: these six tools are what makes a worker able to do a real task, and nothing here
//! reaches around the `tools` seam — each is an ordinary `Tool` registered through
//! `ToolsHandle::register`, guarded by the same pipeline as any other.

pub mod fs;
pub mod invariant;
pub mod spill;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_ledger::{Cite, Ref};
use bough_plugin_tools::{
    FailureClass, PostExecute, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName,
    ToolOutcome, ToolScope, ToolSpec, Tools, ToolsPostExecute, Workspace, WorkspaceRoot,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tools-baseline";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaselineConfig {
    /// The containment root (§7): a check, not a sandbox.
    pub root: PathBuf,
    pub bash_timeout_ms: u64,
    /// Output longer than this spills to a file with a locator inline.
    pub max_output_bytes: usize,
    pub max_read_bytes: usize,
    #[serde(default)]
    pub deny_globs: Vec<String>,
}

/// `bash` — Terminal render, never concurrency-safe.
pub struct Bash(pub Arc<BaselineConfig>);
/// `read_file` — Generic render, concurrency-safe.
pub struct ReadFile(pub Arc<BaselineConfig>);
/// `write_file` — Diff render, not concurrency-safe.
pub struct WriteFile(pub Arc<BaselineConfig>);
/// `edit_file` — Diff render, not concurrency-safe.
pub struct EditFile(pub Arc<BaselineConfig>);
/// `glob` — Generic render, concurrency-safe.
pub struct Glob(pub Arc<BaselineConfig>);
/// `grep` — Generic render, concurrency-safe.
pub struct Grep(pub Arc<BaselineConfig>);

// ---------------------------------------------------------------------------
// helpers shared by the six
// ---------------------------------------------------------------------------

fn err(kind: FailureClass, message: impl Into<String>) -> ToolFailure {
    ToolFailure {
        kind,
        message: message.into(),
    }
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

fn arg_str_opt(call: &ToolCall, key: &str) -> Option<String> {
    call.args
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Resolve one path argument through the containment check AND the deny list.
fn path_arg(cfg: &BaselineConfig, call: &ToolCall, key: &str) -> Result<PathBuf, ToolFailure> {
    let raw = arg_str(call, key)?;
    let p = fs::contain(&cfg.root, &raw).map_err(|m| err(FailureClass::Denied, m))?;
    if fs::denied(&cfg.deny_globs, &p) {
        return Err(err(
            FailureClass::Denied,
            format!("path `{raw}` matches a denied glob"),
        ));
    }
    Ok(p)
}

/// A file cite, so a `read_file` result is EVIDENCE and a `bash` result is not (P2-D26).
fn file_cite(p: &std::path::Path) -> Cite {
    Cite {
        r#ref: Ref::new(format!("file:{}", p.display())),
        url: None,
    }
}

fn schema(v: serde_json::Value) -> schemars::Schema {
    schemars::Schema::try_from(v).expect("a baseline tool's input schema is an object")
}

// ---------------------------------------------------------------------------
// the six
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl Tool for Bash {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let command = arg_str(&call, "command")?;
        let cwd = match arg_str_opt(&call, "cwd") {
            Some(c) => fs::contain(&self.0.root, &c).map_err(|m| err(FailureClass::Denied, m))?,
            // The pinned root is already absolute and canonical (phase ux1 §2.10): resolving it
            // again here is what let a later `chdir` retarget the call.
            None => self.0.root.clone(),
        };
        let timeout = std::time::Duration::from_millis(self.0.bash_timeout_ms);
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| err(FailureClass::Error, format!("could not start `sh`: {e}")))?;

        // `kill_on_drop` is what makes cancellation and timeout real: dropping the future drops
        // the child and kills the process.
        let wait = child.wait_with_output();
        let out = tokio::select! {
            biased;
            _ = cx.cancel.cancelled() => {
                return Err(err(FailureClass::Cancelled, "`bash` was cancelled"));
            }
            r = tokio::time::timeout(timeout, wait) => match r {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => return Err(err(FailureClass::Error, format!("`bash` failed: {e}"))),
                Err(_) => return Err(err(
                    FailureClass::Timeout,
                    format!("`bash` exceeded {}ms", self.0.bash_timeout_ms),
                )),
            },
        };
        let code = out.status.code().unwrap_or(-1);
        let mut content = String::new();
        content.push_str(&String::from_utf8_lossy(&out.stdout));
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.is_empty() {
            content.push_str(&stderr);
        }
        content.push_str(&format!("\n[exit status: {code}]"));
        Ok(ToolOutcome {
            content,
            // No cites: a shell result is a THOUGHT unless something else vouches for it
            // (P2-D26).
            value: Some(serde_json::json!({ "exit_code": code })),
            cites: vec![],
            concludes_wake: false,
        })
    }
}

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let path = path_arg(&self.0, &call, "path")?;
        let bytes = tokio::fs::read(&path).await.map_err(|e| {
            err(
                FailureClass::NotFound,
                format!("cannot read `{}`: {e}", path.display()),
            )
        })?;
        let truncated = bytes.len() > self.0.max_read_bytes;
        let slice = if truncated {
            &bytes[..self.0.max_read_bytes]
        } else {
            &bytes[..]
        };
        let mut content = String::from_utf8_lossy(slice).to_string();
        if truncated {
            content.push_str(&format!(
                "\n[truncated at {} of {} bytes]",
                self.0.max_read_bytes,
                bytes.len()
            ));
        }
        Ok(ToolOutcome {
            content,
            value: None,
            cites: vec![file_cite(&path)],
            concludes_wake: false,
        })
    }
}

#[async_trait::async_trait]
impl Tool for WriteFile {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let path = path_arg(&self.0, &call, "path")?;
        let content = arg_str(&call, "content")?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                err(
                    FailureClass::Error,
                    format!("cannot create `{}`: {e}", parent.display()),
                )
            })?;
        }
        tokio::fs::write(&path, content.as_bytes())
            .await
            .map_err(|e| {
                err(
                    FailureClass::Error,
                    format!("cannot write `{}`: {e}", path.display()),
                )
            })?;
        Ok(ToolOutcome {
            content: format!("wrote {} bytes to {}", content.len(), path.display()),
            value: None,
            cites: vec![file_cite(&path)],
            concludes_wake: false,
        })
    }
}

#[async_trait::async_trait]
impl Tool for EditFile {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let path = path_arg(&self.0, &call, "path")?;
        let old = arg_str(&call, "old")?;
        let new = arg_str(&call, "new")?;
        let before = tokio::fs::read_to_string(&path).await.map_err(|e| {
            err(
                FailureClass::NotFound,
                format!("cannot read `{}`: {e}", path.display()),
            )
        })?;
        let hits = before.matches(&old).count();
        if hits == 0 {
            return Err(err(
                FailureClass::Error,
                format!("`old` does not appear in {}", path.display()),
            ));
        }
        if hits > 1 {
            return Err(err(
                FailureClass::Error,
                format!(
                    "`old` appears {hits} times in {}; make it unique",
                    path.display()
                ),
            ));
        }
        let after = before.replace(&old, &new);
        tokio::fs::write(&path, after.as_bytes())
            .await
            .map_err(|e| {
                err(
                    FailureClass::Error,
                    format!("cannot write `{}`: {e}", path.display()),
                )
            })?;
        Ok(ToolOutcome {
            content: format!("edited {}", path.display()),
            value: None,
            cites: vec![file_cite(&path)],
            concludes_wake: false,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Glob {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let pattern = arg_str(&call, "pattern")?;
        let root = match arg_str_opt(&call, "path") {
            Some(p) => fs::contain(&self.0.root, &p).map_err(|m| err(FailureClass::Denied, m))?,
            // The pinned root is already absolute and canonical (phase ux1 §2.10): resolving it
            // again here is what let a later `chdir` retarget the call.
            None => self.0.root.clone(),
        };
        let re = regex::Regex::new(&fs::glob_to_regex(&pattern))
            .map_err(|e| err(FailureClass::Error, format!("bad pattern: {e}")))?;
        let mut hits: Vec<String> = Vec::new();
        for p in walk(&root) {
            if fs::denied(&self.0.deny_globs, &p) {
                continue;
            }
            let rel = p
                .strip_prefix(&root)
                .unwrap_or(&p)
                .to_string_lossy()
                .to_string();
            if re.is_match(&rel) {
                hits.push(rel);
            }
        }
        hits.sort();
        Ok(ToolOutcome {
            content: if hits.is_empty() {
                "no matches".to_string()
            } else {
                hits.join("\n")
            },
            value: None,
            cites: vec![],
            concludes_wake: false,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Grep {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let pattern = arg_str(&call, "pattern")?;
        let root = match arg_str_opt(&call, "path") {
            Some(p) => fs::contain(&self.0.root, &p).map_err(|m| err(FailureClass::Denied, m))?,
            // The pinned root is already absolute and canonical (phase ux1 §2.10): resolving it
            // again here is what let a later `chdir` retarget the call.
            None => self.0.root.clone(),
        };
        let re = regex::Regex::new(&pattern)
            .map_err(|e| err(FailureClass::Error, format!("bad regex: {e}")))?;
        let files = if root.is_file() {
            vec![root.clone()]
        } else {
            walk(&root)
        };
        let mut lines: Vec<String> = Vec::new();
        let mut cites: Vec<Cite> = Vec::new();
        for p in files {
            if fs::denied(&self.0.deny_globs, &p) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let mut matched = false;
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    matched = true;
                    let rel = p
                        .strip_prefix(&root)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .to_string();
                    lines.push(format!("{rel}:{}:{line}", i + 1));
                }
            }
            if matched {
                cites.push(file_cite(&p));
            }
        }
        lines.sort();
        Ok(ToolOutcome {
            content: if lines.is_empty() {
                "no matches".to_string()
            } else {
                lines.join("\n")
            },
            value: None,
            cites,
            concludes_wake: false,
        })
    }
}

/// Every file under `root`, depth-first, symlinks not followed.
fn walk(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => out.push(p),
                _ => {}
            }
        }
    }
    out
}

/// The six specs this row registers, with their render intents.
pub fn specs(cfg: Arc<BaselineConfig>) -> Vec<ToolSpec> {
    let path_prop = serde_json::json!({ "type": "string" });
    vec![
        ToolSpec {
            name: ToolName::new("bash"),
            description: "Run a shell command in the task tree and return its output and exit \
                          status. `tags` is 3-5 short lowercase words naming the tool, the intent \
                          and the subject (`[\"cargo\", \"test\", \"focus\"]`): they index the \
                          command in the cross-session history, which is the only way a later \
                          session finds it."
                .into(),
            // MERGE: `tags` is a DECLARED property, and it is in `required` before `cwd` because
            // the code-mode surface binds arguments positionally in `required`-then-sorted order
            // (`tools-codemode::bind::positional_order`) — that is what makes the injected
            // signature the documented `bash(cmd, tags)` rather than `bash(cmd, cwd)`.
            // `docs/codemode-merge-notes.md` §9 is the whole story.
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "command": path_prop,
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 3,
                        "maxItems": 5
                    },
                    "cwd": path_prop
                },
                "required": ["command", "tags"]
            })),
            render: RenderIntent::Terminal,
            scope: ToolScope::Global,
            tool: Arc::new(Bash(cfg.clone())),
        },
        ToolSpec {
            name: ToolName::new("read_file"),
            description: "Read a file under the task tree.".into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "path": path_prop },
                "required": ["path"]
            })),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(ReadFile(cfg.clone())),
        },
        ToolSpec {
            name: ToolName::new("write_file"),
            description: "Write a file under the task tree, creating parent directories.".into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "path": path_prop, "content": path_prop },
                "required": ["path", "content"]
            })),
            render: RenderIntent::Diff,
            scope: ToolScope::Global,
            tool: Arc::new(WriteFile(cfg.clone())),
        },
        ToolSpec {
            name: ToolName::new("edit_file"),
            description: "Replace one unique occurrence of `old` with `new` in a file.".into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "path": path_prop, "old": path_prop, "new": path_prop },
                "required": ["path", "old", "new"]
            })),
            render: RenderIntent::Diff,
            scope: ToolScope::Global,
            tool: Arc::new(EditFile(cfg.clone())),
        },
        ToolSpec {
            name: ToolName::new("glob"),
            description: "List files under the task tree matching a glob pattern.".into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "pattern": path_prop, "path": path_prop },
                "required": ["pattern"]
            })),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(Glob(cfg.clone())),
        },
        ToolSpec {
            name: ToolName::new("grep"),
            description: "Search files under the task tree for a regular expression.".into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "pattern": path_prop, "path": path_prop },
                "required": ["pattern"]
            })),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(Grep(cfg)),
        },
    ]
}

/// Named so the render intents are visible in the scaffold rather than only in `specs`.
pub const RENDER_INTENTS: &[(&str, RenderIntent)] = &[
    ("bash", RenderIntent::Terminal),
    ("read_file", RenderIntent::Generic),
    ("write_file", RenderIntent::Diff),
    ("edit_file", RenderIntent::Diff),
    ("glob", RenderIntent::Generic),
    ("grep", RenderIntent::Generic),
];

/// The consumer row.
pub struct BaselineToolsPlugin;

#[async_trait::async_trait]
impl Plugin for BaselineToolsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = BaselineConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.max_output_bytes == 0 || cfg.max_read_bytes == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "max_output_bytes and max_read_bytes must be at least 1".to_string(),
            });
        }
        if cfg.bash_timeout_ms == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "bash_timeout_ms must be at least 1".to_string(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tools = ctx
            .get::<Tools>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // phase ux1 §2.10 (B5): the root is resolved ONCE, HERE, against the process cwd this
        // binary booted in — and then published, so the directory the tools use and the one the
        // status line shows are the same object. A root that does not exist is a LOAD failure.
        let cwd = std::env::current_dir()
            .map_err(|e| PluginError::new(entry.clone(), anyhow::anyhow!("no process cwd: {e}")))?;
        let pinned = fs::pin_root(&cfg.root, &cwd)
            .map_err(|m| PluginError::new(entry.clone(), anyhow::anyhow!(m)))?;
        ctx.provide::<Workspace>(
            WorkspaceRoot::new(pinned.clone())
                .map_err(|e| PluginError::new(ctx.entry_id().clone(), anyhow::anyhow!(e)))?,
        )
        .await
        .map_err(|e| PluginError::new(entry.clone(), e))?;
        // Every tool holds the PINNED root, never the configured spelling of it.
        let cfg = Arc::new(BaselineConfig {
            root: pinned,
            ..(*cfg).clone()
        });

        for spec in specs(cfg.clone()) {
            tools.register(&ctx, spec).await?;
        }
        // §9's named example: the spill listener is an ordinary `tools/post-execute` listener,
        // registered as an effect like everything else.
        let max_output_bytes = cfg.max_output_bytes;
        ctx.on_waterfall::<ToolsPostExecute, _, _>(move |mut post: PostExecute, next| async move {
            spill::spill_if_oversized(max_output_bytes, &mut post);
            next.run(post).await
        })
        .await?;
        Ok(())
    }
}

bough_kernel::register_plugin!(BaselineToolsPlugin);

#[cfg(test)]
mod config_tests {
    use super::*;

    fn cfg() -> BaselineConfig {
        BaselineConfig {
            root: PathBuf::from("/tmp"),
            bash_timeout_ms: 1000,
            max_output_bytes: 100,
            max_read_bytes: 100,
            deny_globs: vec![],
        }
    }

    #[test]
    fn a_zero_bound_is_rejected() {
        let mut c = cfg();
        c.max_output_bytes = 0;
        assert!(BaselineToolsPlugin::validate(&c).is_err());
        let mut c = cfg();
        c.bash_timeout_ms = 0;
        assert!(BaselineToolsPlugin::validate(&c).is_err());
        assert!(BaselineToolsPlugin::validate(&cfg()).is_ok());
    }

    #[test]
    fn the_six_are_registered_with_their_render_intents() {
        let specs = specs(Arc::new(cfg()));
        assert_eq!(specs.len(), 6);
        for (name, render) in RENDER_INTENTS {
            let s = specs
                .iter()
                .find(|s| s.name.as_str() == *name)
                .unwrap_or_else(|| panic!("`{name}` is not registered"));
            assert_eq!(s.render, *render, "{name}");
        }
    }
}
