//! `bough mind <verb>` — the headless client for the mind surface
//! (specs/mind.md §8).
//!
//! Conventions are `mcp.rs`'s: parsing is pure and total, every effect is
//! injected, `run_mind` RETURNS an exit code:
//!
//!   0  the verb did what it says
//!   1  the operation ran and the answer is bad
//!   2  usage problem, or no server on the port
//!
//! `new` prints the warning that specs/mind.md §9 assigns to it: a mind runs
//! unattended with the session's full authority in its workspace. That line
//! is product surface, not decoration — it is the one moment the human is
//! guaranteed to be present.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

const VERBS: [&str; 6] = ["new", "list", "start", "stop", "status", "steps"];
const NEEDS_ID: [&str; 4] = ["start", "stop", "status", "steps"];

#[derive(Debug, Clone, PartialEq)]
pub struct MindArgs {
    pub verb: String,
    /// The mind session the verb acts on. Absent for `new` and `list`.
    pub id: Option<String>,
    /// `new` only.
    pub workspace: Option<String>,
    pub persona: Option<String>,
    pub title: Option<String>,
    pub port: Option<u32>,
}

pub fn usage() -> String {
    concat!(
        "usage: bough mind <verb> [args]\n",
        "\n",
        "  new [--workspace DIR] [--persona TEXT] [--title T]\n",
        "                       create a mind session (disabled until start)\n",
        "  list                 every mind, with its loop state\n",
        "  start ID             enable the wake loop (first wake within ~30s)\n",
        "  stop ID              disable it (a running wakeup still finishes)\n",
        "  status ID            enabled, streaks, next wake, step count\n",
        "  steps ID [-n N]      the recent stream, oldest first (default 50)\n",
        "\n",
        "  --port N             server port (default BOUGH_PORT, else 4321)\n",
    )
    .to_string()
}

/// Pure and total: every argv is either `Ok(MindArgs)` or `Err(usage text)`.
pub fn parse_mind_args(argv: &[String]) -> Result<MindArgs, String> {
    let mut verb: Option<String> = None;
    let mut id: Option<String> = None;
    let mut workspace = None;
    let mut persona = None;
    let mut title = None;
    let mut port = None;
    let mut n_flag: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "--workspace" | "--persona" | "--title" | "--port" | "-n" => {
                let Some(v) = argv.get(i + 1) else {
                    return Err(format!("{a} needs a value\n\n{}", usage()));
                };
                match a {
                    "--workspace" => workspace = Some(v.clone()),
                    "--persona" => persona = Some(v.clone()),
                    "--title" => title = Some(v.clone()),
                    "-n" => n_flag = Some(v.clone()),
                    _ => {
                        port = Some(
                            v.parse()
                                .map_err(|_| format!("--port is not a number: {v}"))?,
                        )
                    }
                }
                i += 2;
            }
            "-h" | "--help" => return Err(usage()),
            _ if a.starts_with('-') => return Err(format!("unknown flag {a}\n\n{}", usage())),
            _ if verb.is_none() => {
                if !VERBS.contains(&a) {
                    return Err(format!("unknown verb {a}\n\n{}", usage()));
                }
                verb = Some(a.to_string());
                i += 1;
            }
            _ if id.is_none() => {
                id = Some(a.to_string());
                i += 1;
            }
            _ => return Err(format!("unexpected argument {a}\n\n{}", usage())),
        }
    }
    let Some(verb) = verb else {
        return Err(usage());
    };
    if NEEDS_ID.contains(&verb.as_str()) && id.is_none() {
        return Err(format!("{verb} needs a session id\n\n{}", usage()));
    }
    // `-n` rides in `title` for steps — a private slot, never printed.
    if verb == "steps" {
        title = n_flag;
    }
    Ok(MindArgs {
        verb,
        id,
        workspace,
        persona,
        title,
        port,
    })
}

// ---- injected effects (mcp.rs's shapes) -------------------------------------

pub struct MindRequest {
    pub method: String,
    pub url: String,
    pub body: Option<String>,
}

pub struct MindResponse {
    pub status: u16,
    pub text: String,
}

pub type MindFetch =
    Arc<dyn Fn(MindRequest) -> BoxFuture<'static, Result<MindResponse, String>> + Send + Sync>;

pub struct MindDeps {
    pub fetch: MindFetch,
    pub out: Arc<dyn Fn(&str) + Send + Sync>,
    pub err: Arc<dyn Fn(&str) + Send + Sync>,
    pub env: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

pub fn real_deps() -> MindDeps {
    let client = reqwest::Client::new();
    MindDeps {
        fetch: Arc::new(move |req: MindRequest| {
            let client = client.clone();
            Box::pin(async move {
                let method = reqwest::Method::from_bytes(req.method.as_bytes())
                    .map_err(|e| e.to_string())?;
                let mut b = client.request(method, &req.url);
                if let Some(body) = req.body {
                    b = b.header("content-type", "application/json").body(body);
                }
                let res = b.send().await.map_err(|e| e.to_string())?;
                let status = res.status().as_u16();
                let text = res.text().await.map_err(|e| e.to_string())?;
                Ok(MindResponse { status, text })
            })
        }),
        out: Arc::new(|line| println!("{line}")),
        err: Arc::new(|line| eprintln!("{line}")),
        env: Arc::new(|name| std::env::var(name).ok()),
    }
}

fn base(args: &MindArgs, deps: &MindDeps) -> String {
    let port = args.port.unwrap_or_else(|| {
        (deps.env)("BOUGH_PORT")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(4321)
    });
    format!("http://127.0.0.1:{port}")
}

struct Answer {
    status: u16,
    body: Value,
}

fn error_of(r: &Answer) -> String {
    r.body["error"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("HTTP {}", r.status))
}

async fn call(deps: &MindDeps, method: &str, url: String, body: Option<Value>) -> Option<Answer> {
    let req = MindRequest {
        method: method.into(),
        url: url.clone(),
        body: body.map(|v| v.to_string()),
    };
    match (deps.fetch)(req).await {
        Ok(res) => {
            let body = if res.text.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&res.text).unwrap_or_else(|_| json!({ "error": res.text }))
            };
            Some(Answer {
                status: res.status,
                body,
            })
        }
        Err(e) => {
            (deps.err)(&format!("no bough server ({e}). Start one: bough start"));
            None
        }
    }
}

fn status_line(id: &str, title: &str, s: &Value) -> String {
    let enabled = s["enabled"].as_bool().unwrap_or(false);
    let steps = s["stepCount"].as_i64().unwrap_or(0);
    let idle = s["idleStreak"].as_i64().unwrap_or(0);
    let fails = s["failStreak"].as_i64().unwrap_or(0);
    let state = if s["pending"].as_bool() == Some(true) {
        "waking"
    } else if enabled {
        "enabled"
    } else {
        "stopped"
    };
    let mut line = format!("{id}  {state}  {steps} steps  idle×{idle}");
    if fails > 0 {
        line.push_str(&format!("  fails×{fails}"));
    }
    if !title.is_empty() {
        line.push_str("  ");
        line.push_str(title);
    }
    line
}

pub async fn run_mind(argv: &[String], deps: &MindDeps) -> i32 {
    let args = match parse_mind_args(argv) {
        Ok(a) => a,
        Err(text) => {
            (deps.err)(&text);
            return 2;
        }
    };
    let base = base(&args, deps);

    match args.verb.as_str() {
        "new" => {
            let mut body = json!({ "kind": "mind" });
            if let Some(w) = &args.workspace {
                body["workspace"] = json!(w);
            }
            if let Some(t) = &args.title {
                body["title"] = json!(t);
            }
            let Some(r) = call(deps, "POST", format!("{base}/sessions"), Some(body)).await else {
                return 2;
            };
            if r.status >= 300 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            let id = r.body["id"].as_str().unwrap_or_default().to_string();
            if let Some(p) = &args.persona {
                let Some(r) = call(
                    deps,
                    "POST",
                    format!("{base}/sessions/{id}/mind"),
                    Some(json!({ "persona": p })),
                )
                .await
                else {
                    return 2;
                };
                if r.status >= 300 {
                    (deps.err)(&error_of(&r));
                    return 1;
                }
            }
            (deps.out)(&id);
            (deps.out)(
                "created, stopped. `bough mind start` begins the loop — it will then run \
UNATTENDED with your full authority in its workspace, on your API key, until \
`bough mind stop`. Point it at a checkout you are willing to review.",
            );
            0
        }
        "list" => {
            let Some(r) = call(deps, "GET", format!("{base}/sessions"), None).await else {
                return 2;
            };
            if r.status >= 300 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            let minds: Vec<Value> = r
                .body
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter(|s| s["kind"] == "mind")
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if minds.is_empty() {
                (deps.out)("no minds. `bough mind new` creates one.");
                return 0;
            }
            for m in minds {
                let id = m["id"].as_str().unwrap_or_default().to_string();
                let title = m["title"].as_str().unwrap_or_default().to_string();
                let Some(s) = call(deps, "GET", format!("{base}/sessions/{id}/mind"), None).await
                else {
                    return 2;
                };
                (deps.out)(&status_line(&id, &title, &s.body));
            }
            0
        }
        "start" | "stop" => {
            let id = args.id.clone().unwrap_or_default();
            let enabled = args.verb == "start";
            let Some(r) = call(
                deps,
                "POST",
                format!("{base}/sessions/{id}/mind"),
                Some(json!({ "enabled": enabled })),
            )
            .await
            else {
                return 2;
            };
            if r.status >= 300 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            (deps.out)(if enabled {
                "enabled — first wake within ~30s"
            } else {
                "stopped — a running wakeup still finishes, then nothing wakes it"
            });
            0
        }
        "status" => {
            let id = args.id.clone().unwrap_or_default();
            let Some(r) = call(deps, "GET", format!("{base}/sessions/{id}/mind"), None).await
            else {
                return 2;
            };
            if r.status >= 300 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            (deps.out)(&serde_json::to_string_pretty(&r.body).unwrap_or_default());
            0
        }
        "steps" => {
            let id = args.id.clone().unwrap_or_default();
            let n = args
                .title // `-n` rides here, see parse
                .as_deref()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(50);
            let Some(r) = call(
                deps,
                "GET",
                format!("{base}/sessions/{id}/mind/steps?n={n}"),
                None,
            )
            .await
            else {
                return 2;
            };
            if r.status >= 300 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            for s in r.body.as_array().cloned().unwrap_or_default() {
                (deps.out)(&format!(
                    "[#{} {} · {}] {}",
                    s["id"],
                    s["type"].as_str().unwrap_or("?"),
                    s["source"].as_str().unwrap_or("?"),
                    s["content"].as_str().unwrap_or_default()
                ));
            }
            0
        }
        _ => unreachable!("parse rejects unknown verbs"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn deps_with(
        routes: Vec<(&'static str, &'static str, u16, Value)>,
    ) -> (MindDeps, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
        let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let errs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let out_c = out.clone();
        let err_c = errs.clone();
        let deps = MindDeps {
            fetch: Arc::new(move |req: MindRequest| {
                let routes = routes.clone();
                Box::pin(async move {
                    for (method, suffix, status, body) in &routes {
                        if req.method == *method && req.url.contains(suffix) {
                            return Ok(MindResponse {
                                status: *status,
                                text: body.to_string(),
                            });
                        }
                    }
                    Ok(MindResponse {
                        status: 404,
                        text: "{\"error\":\"no such route\"}".into(),
                    })
                })
            }),
            out: Arc::new(move |l| out_c.lock().unwrap().push(l.to_string())),
            err: Arc::new(move |l| err_c.lock().unwrap().push(l.to_string())),
            env: Arc::new(|_| None),
        };
        (deps, out, errs)
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parsing_is_total_and_the_id_verbs_demand_an_id() {
        assert!(parse_mind_args(&argv(&["status"])).is_err());
        assert!(parse_mind_args(&argv(&["dance"])).is_err());
        let a = parse_mind_args(&argv(&[
            "new",
            "--persona",
            "curious",
            "--workspace",
            "/tmp/x",
        ]))
        .unwrap();
        assert_eq!(a.verb, "new");
        assert_eq!(a.persona.as_deref(), Some("curious"));
        let a = parse_mind_args(&argv(&["steps", "abc", "-n", "5"])).unwrap();
        assert_eq!(a.id.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn new_creates_sets_persona_and_prints_the_warning() {
        let (deps, out, _) = deps_with(vec![
            ("POST", "/sessions/s1/mind", 200, serde_json::json!({"enabled": false})),
            ("POST", "/sessions", 201, serde_json::json!({"id": "s1"})),
        ]);
        let code = run_mind(&argv(&["new", "--persona", "curious"]), &deps).await;
        assert_eq!(code, 0);
        let lines = out.lock().unwrap();
        assert_eq!(lines[0], "s1");
        assert!(lines[1].contains("UNATTENDED"), "the warning is the product");
    }

    #[tokio::test]
    async fn start_flips_the_switch_and_a_route_error_is_exit_1() {
        let (deps, out, _) = deps_with(vec![(
            "POST",
            "/sessions/s1/mind",
            200,
            serde_json::json!({"enabled": true}),
        )]);
        assert_eq!(run_mind(&argv(&["start", "s1"]), &deps).await, 0);
        assert!(out.lock().unwrap()[0].contains("first wake"));

        let (deps, _, errs) = deps_with(vec![(
            "POST",
            "/sessions/root1/mind",
            400,
            serde_json::json!({"error": "session root1 is not a mind"}),
        )]);
        assert_eq!(run_mind(&argv(&["start", "root1"]), &deps).await, 1);
        assert!(errs.lock().unwrap()[0].contains("not a mind"));
    }

    #[tokio::test]
    async fn steps_render_the_stream_one_line_each() {
        let (deps, out, _) = deps_with(vec![(
            "GET",
            "/sessions/s1/mind/steps",
            200,
            serde_json::json!([
                {"id": 1, "type": "thought", "source": "self", "content": "hm"},
                {"id": 2, "type": "idle", "source": "self", "content": "idle"}
            ]),
        )]);
        assert_eq!(run_mind(&argv(&["steps", "s1"]), &deps).await, 0);
        let lines = out.lock().unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("thought"));
    }
}
