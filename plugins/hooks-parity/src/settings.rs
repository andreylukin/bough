//! Invariant: discovery is a PURE function of a directory chain — the sources for a call are the
//! user layer plus every ancestor of the call's OWN cwd, outermost first — and a FOREIGN settings
//! file that fails to parse is warned about and skipped, never fatal: a repo with a broken
//! `settings.json` must not brick tool dispatch.

use std::path::{Path, PathBuf};

/// One hook, as either format spells it: an event, an optional tool-name regex, a command.
#[derive(Clone, Debug, PartialEq)]
pub struct HookDef {
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    /// From the file, in ms (both formats spell seconds). `None` = the row's default.
    pub timeout_ms: Option<u64>,
    /// The settings file this came from, for attribution in reasons and logs.
    pub source: PathBuf,
}

/// The files one call's cwd implies, in PRECEDENCE order: the user layer, then each ancestor
/// outermost-first. Claude Code reads `settings.json` + `settings.local.json` under `.claude`;
/// Codex reads `hooks.json` + `config.toml` under `.codex`. A path listed twice is listed once.
pub fn source_files(
    cwd: &Path,
    home: Option<&Path>,
    claude: bool,
    codex: bool,
    user_layer: bool,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    if user_layer {
        if let Some(h) = home {
            if claude {
                push(h.join(".claude/settings.json"));
            }
            if codex {
                push(h.join(".codex/hooks.json"));
                push(h.join(".codex/config.toml"));
            }
        }
    }
    let mut chain: Vec<&Path> = cwd.ancestors().collect();
    chain.reverse();
    for dir in chain {
        if claude {
            push(dir.join(".claude/settings.json"));
            push(dir.join(".claude/settings.local.json"));
        }
        if codex {
            push(dir.join(".codex/hooks.json"));
            push(dir.join(".codex/config.toml"));
        }
    }
    out
}

/// Read and parse every existing source file for `cwd`. Missing files are absent; unreadable or
/// malformed ones are warned about and skipped.
pub fn discover(
    cwd: &Path,
    home: Option<&Path>,
    claude: bool,
    codex: bool,
    user_layer: bool,
) -> Vec<HookDef> {
    let mut out = Vec::new();
    for path in source_files(cwd, home, claude, codex, user_layer) {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "hook settings unreadable");
                continue;
            }
        };
        match parse_source(&path, &text) {
            Ok(defs) => out.extend(defs),
            Err(detail) => {
                tracing::warn!(file = %path.display(), %detail, "hook settings unparsable");
            }
        }
    }
    out
}

/// Parse one file by what it is: `config.toml` as a TOML `[hooks]` table, everything else as
/// JSON — a Claude `settings.json` (hooks under the `hooks` key) or a Codex `hooks.json` (same).
pub fn parse_source(path: &Path, text: &str) -> Result<Vec<HookDef>, String> {
    let value: serde_json::Value = if path.extension().is_some_and(|e| e == "toml") {
        let t: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        serde_json::to_value(t).map_err(|e| e.to_string())?
    } else {
        serde_json::from_str(text).map_err(|e| e.to_string())?
    };
    let Some(hooks) = value.get("hooks") else {
        return Ok(Vec::new());
    };
    Ok(from_hooks_value(hooks, path))
}

/// The shared three-level walk both formats nest:
/// `{ "<Event>": [ { matcher?, hooks: [ { type?, command, timeout? } ] } ] }`.
/// Only `type: command` hooks are understood; anything else is skipped with a debug log.
pub fn from_hooks_value(hooks: &serde_json::Value, source: &Path) -> Vec<HookDef> {
    let mut out = Vec::new();
    let Some(map) = hooks.as_object() else {
        return out;
    };
    for (event, entries) in map {
        let Some(entries) = entries.as_array() else {
            continue;
        };
        for entry in entries {
            let matcher = entry
                .get("matcher")
                .and_then(|m| m.as_str())
                .filter(|m| !m.is_empty())
                .map(str::to_string);
            let Some(cmds) = entry.get("hooks").and_then(|h| h.as_array()) else {
                continue;
            };
            for h in cmds {
                let ty = h.get("type").and_then(|t| t.as_str()).unwrap_or("command");
                if ty != "command" {
                    tracing::debug!(%ty, file = %source.display(), "hook type not supported");
                    continue;
                }
                let Some(command) = h.get("command").and_then(|c| c.as_str()) else {
                    continue;
                };
                out.push(HookDef {
                    event: event.clone(),
                    matcher: matcher.clone(),
                    command: command.to_string(),
                    timeout_ms: h
                        .get("timeout")
                        .and_then(|t| t.as_f64())
                        .map(|s| (s * 1000.0) as u64),
                    source: source.to_path_buf(),
                });
            }
        }
    }
    out
}

/// PURE: the hooks that fire for one (event, tool) — the row's toggles, then the event, then the
/// matcher regex tried against every given spelling of the tool's name (the raw bough name and
/// its parity alias). An invalid regex is warned about and never fires.
pub fn filtered<'a>(
    defs: &'a [HookDef],
    event: &str,
    names: &[&str],
    events_on: &[String],
    only: &[String],
    except: &[String],
) -> Vec<&'a HookDef> {
    if !events_on.is_empty() && !events_on.iter().any(|e| e == event) {
        return Vec::new();
    }
    defs.iter()
        .filter(|d| d.event == event)
        .filter(|d| only.is_empty() || only.iter().any(|o| d.command.contains(o)))
        .filter(|d| !except.iter().any(|x| d.command.contains(x)))
        .filter(|d| match &d.matcher {
            None => true,
            Some(m) => match regex::Regex::new(m) {
                Ok(re) => names.iter().any(|n| re.is_match(n)),
                Err(e) => {
                    tracing::warn!(matcher = %m, error = %e, "hook matcher is not a valid regex");
                    false
                }
            },
        })
        .collect()
}

/// PURE: the directory one call runs in — `args.cwd`, else the directory of a path-shaped
/// argument, else the workspace root, else the process cwd. Relative spellings resolve against
/// the workspace, exactly as the shell tool resolves them.
pub fn call_cwd(args: &serde_json::Value, workspace: Option<&Path>) -> PathBuf {
    let abs = |p: PathBuf| -> Option<PathBuf> {
        if p.as_os_str().is_empty() {
            None
        } else if p.is_absolute() {
            Some(p)
        } else {
            workspace.map(|w| w.join(p))
        }
    };
    if let Some(c) = args.get("cwd").and_then(|v| v.as_str()) {
        if let Some(p) = abs(PathBuf::from(c)) {
            return p;
        }
    }
    for key in ["path", "file_path", "file"] {
        if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
            if let Some(p) = abs(PathBuf::from(s)) {
                return p.parent().map(Path::to_path_buf).unwrap_or(p);
            }
        }
    }
    workspace
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}
