//! Invariant: caps are enforced by the RUNTIME, not by the program. `set_memory_limit`,
//! `set_max_stack_size` and an interrupt handler that counts ops and samples the wall clock are
//! set before a single byte of the program's source is evaluated. The runtime has no loader, no
//! `std`/`os` bindings and no timers: the ONLY capabilities a program has are the `HostFn`s the
//! seam handed it, and every one of them is a bridged call back into the pipeline.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bough_plugin_js::{JsEngine, JsError, Program, RefusalKind, Run};
use rquickjs::function::{Async, Func};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, CaughtError, Function, Value};

use crate::{invariant, preflight, QuickJsConfig};

/// Why the interrupt handler stopped the program. `0` means "it did not".
mod reason {
    pub const NONE: u8 = 0;
    pub const OPS: u8 = 1;
    pub const TIME: u8 = 2;
    pub const CANCEL: u8 = 3;
}

/// The rquickjs engine. One `Runtime` per program, dropped after.
pub struct QuickJsEngine {
    cfg: Arc<QuickJsConfig>,
    /// The barrier that enforces `max_concurrent_programs`.
    slots: Arc<tokio::sync::Semaphore>,
}

impl QuickJsEngine {
    pub fn new(cfg: Arc<QuickJsConfig>) -> QuickJsEngine {
        let slots = Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrent_programs));
        QuickJsEngine { cfg, slots }
    }
}

#[async_trait::async_trait]
impl JsEngine for QuickJsEngine {
    fn name(&self) -> &'static str {
        "quickjs"
    }

    /// Parse only, through the SAME engine that will run the program, so host and engine can
    /// never disagree about what is legal. The source is wrapped in an async function
    /// EXPRESSION that is never called: the whole body is parsed, nothing is executed.
    async fn check(&self, src: &str) -> Result<(), JsError> {
        let src = src.to_string();
        let cfg = self.cfg.clone();
        on_a_js_thread(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");
            rt.block_on(async move { parse_only(&src, &[], &cfg).await })
        })
        .await
    }

    /// Run the program wrapped in an async IIFE — `(async () => { <source> })()` — so top-level
    /// `await` works without any module machinery.
    async fn run(&self, p: Program) -> Result<Run, JsError> {
        let _permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("the slot semaphore is never closed");
        let cfg = self.cfg.clone();
        // Host functions run on the CALLER's runtime, not on the JS thread's: they reach back
        // into the tools pipeline, which owns timers and spawned work of its own.
        let host_rt = tokio::runtime::Handle::current();
        on_a_js_thread(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run_one(p, cfg, host_rt))
        })
        .await
    }
}

/// Run `f` on a thread of its own. The QuickJS runtime is not `Send`, so it never leaves it.
async fn on_a_js_thread<T, F>(f: F) -> Result<T, JsError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, JsError> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("bough-js".into())
        .stack_size(4 << 20)
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| JsError::Thrown {
            message: format!("could not start a JS thread: {e}"),
            stack: None,
        })?;
    rx.await.unwrap_or(Err(JsError::Thrown {
        message: "the JS thread died without an outcome".into(),
        stack: None,
    }))
}

/// A runtime with the caps set and nothing else. The `Guard` keeps the live-runtime count of the
/// invariant honest even when a program ends by panic or early return.
struct Guard;

impl Guard {
    fn open() -> Guard {
        invariant::opened();
        Guard
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        invariant::closed();
    }
}

/// Compile-only, on a runtime that is thrown away.
async fn parse_only(src: &str, bound: &[String], cfg: &QuickJsConfig) -> Result<(), JsError> {
    let _guard = Guard::open();
    let rt = AsyncRuntime::new().map_err(thrown)?;
    rt.set_memory_limit(64 << 20).await;
    rt.set_max_stack_size(1 << 20).await;
    // Even a parse gets a budget: a pathological source must not wedge the thread.
    let ticks = AtomicU64::new(0);
    let budget = cfg.interrupt_check_ops.saturating_mul(10_000);
    rt.set_interrupt_handler(Some(Box::new(move || {
        ticks.fetch_add(1, Ordering::Relaxed) > budget
    })))
    .await;
    let ctx = AsyncContext::full(&rt).await.map_err(thrown)?;
    let src = src.to_string();
    let bound = bound.to_vec();
    let out: Result<(), JsError> = ctx
        .async_with(async |ctx| {
            let wrapped = format!("(async () => {{\n{src}\n}})");
            match ctx.eval::<Value, _>(wrapped.as_bytes()).catch(&ctx) {
                Ok(_) => Ok(()),
                Err(e) => Err(syntax(&e, &src, &bound)),
            }
        })
        .await;
    out
}

/// One program, start to terminal outcome.
async fn run_one(
    p: Program,
    cfg: Arc<QuickJsConfig>,
    host_rt: tokio::runtime::Handle,
) -> Result<Run, JsError> {
    let _guard = Guard::open();
    let started = Instant::now();
    let caps = p.caps;
    let bound: Vec<String> = p.host.iter().map(|h| h.name.clone()).collect();

    let rt = AsyncRuntime::new().map_err(thrown)?;
    rt.set_memory_limit(caps.memory_bytes).await;
    rt.set_max_stack_size(caps.stack_bytes).await;

    // The interrupt handler is the whole of the ops and wall-clock enforcement. It counts every
    // tick, and every `interrupt_check_ops` ticks it also samples the clock and the cancel
    // token — the two things that cannot be counted.
    let why = Arc::new(AtomicU8::new(reason::NONE));
    let ops = Arc::new(AtomicU64::new(0));
    {
        let (why, ops) = (why.clone(), ops.clone());
        let cancel = p.cancel.clone();
        let every = cfg.interrupt_check_ops;
        let deadline = started + Duration::from_millis(caps.wall_ms);
        let budget = caps.ops;
        rt.set_interrupt_handler(Some(Box::new(move || {
            let n = ops.fetch_add(1, Ordering::Relaxed) + 1;
            if n > budget {
                why.store(reason::OPS, Ordering::SeqCst);
                return true;
            }
            if n % every == 0 {
                if cancel.is_cancelled() {
                    why.store(reason::CANCEL, Ordering::SeqCst);
                    return true;
                }
                if Instant::now() >= deadline {
                    why.store(reason::TIME, Ordering::SeqCst);
                    return true;
                }
            }
            false
        })))
        .await;
    }

    let ctx = AsyncContext::full(&rt).await.map_err(thrown)?;

    // console.log lands here: the buffer that becomes `Run::console`, and the sink the consumer
    // flushes as steps. Capped in BYTES, with the overflow counted rather than hidden.
    let console = Rc::new(RefCell::new(Console {
        buf: String::new(),
        dropped: 0,
        cap: caps.console_bytes,
        sink: p.console.clone(),
    }));

    let source = p.source.clone();
    let host = p.host;
    let cancel = p.cancel.clone();

    let console_for_js = console.clone();
    let bound_for_js = bound.clone();
    let program = ctx.async_with(async |ctx| {
        let console = console_for_js;
        let bound = bound_for_js;
        // `__log`: the one synchronous bridge. Formatting is done in JS (below).
        let logger = console.clone();
        ctx.globals()
            .set(
                "__log",
                Func::from(move |line: String| {
                    logger.borrow_mut().write(&line);
                }),
            )
            .catch(&ctx)
            .map_err(|e| thrown_js(&e))?;

        // Every host function is one `__host(name, argsJson) -> Promise<resultJson>`.
        // JSON on the wire keeps `Value<'js>` out of the futures entirely, which is what
        // lets the host body be a plain `'static` future run on the caller's runtime.
        let table: std::collections::HashMap<String, Arc<dyn bough_plugin_js::HostCall>> = host
            .iter()
            .map(|h| (h.name.clone(), h.body.clone()))
            .collect();
        let table = Arc::new(table);
        let rt_handle = host_rt.clone();
        ctx.globals()
            .set(
                "__host",
                Func::from(Async(move |name: String, args_json: String| {
                    let table = table.clone();
                    let rt_handle = rt_handle.clone();
                    async move {
                        let out = call_host(table, name, args_json, rt_handle).await;
                        Ok::<String, rquickjs::Error>(out)
                    }
                })),
            )
            .catch(&ctx)
            .map_err(|e| thrown_js(&e))?;

        // The prelude builds `console` and the namespace tree over `__host`. It is the
        // only JS this crate ships, and it adds no capability of its own.
        ctx.eval::<(), _>(PRELUDE.as_bytes())
            .catch(&ctx)
            .map_err(|e| thrown_js(&e))?;
        let names: Vec<String> = bound.clone();
        let bind: Function = ctx.globals().get("__bind").map_err(thrown)?;
        bind.call::<_, ()>((names,))
            .catch(&ctx)
            .map_err(|e| thrown_js(&e))?;

        // Top-level `await` without module machinery: an async IIFE.
        let wrapped = format!("(async () => {{\n{source}\n}})()");
        let promise: rquickjs::Promise = match ctx
            .eval::<rquickjs::Promise, _>(wrapped.as_bytes())
            .catch(&ctx)
        {
            Ok(pr) => pr,
            Err(e) => return Err(syntax_or_thrown(&e, &source, &bound)),
        };
        let value: Result<serde_json::Value, JsError> =
            match promise.into_future::<Value>().await.catch(&ctx) {
                Ok(v) => Ok(to_json(&ctx, v)),
                Err(e) => Err(thrown_js(&e)),
            };
        value
    });

    // The interrupt handler cannot see a program parked in `await host()`, so cancellation and
    // the wall clock are ALSO enforced out here, around the whole future.
    let left = Duration::from_millis(caps.wall_ms).saturating_sub(started.elapsed());
    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(JsError::Cancelled),
        _ = tokio::time::sleep(left) => Err(JsError::TimeExceeded { ms: caps.wall_ms }),
        v = program => v,
    };

    // Whatever happened, the interrupt handler's verdict is the more specific one.
    let outcome = match why.load(Ordering::SeqCst) {
        reason::OPS => Err(JsError::OpsExceeded { ops: caps.ops }),
        reason::TIME => Err(JsError::TimeExceeded { ms: caps.wall_ms }),
        reason::CANCEL => Err(JsError::Cancelled),
        _ => outcome,
    };
    // A cancel that landed while a host call was in flight beats a derived error.
    let outcome = if cancel.is_cancelled() {
        Err(JsError::Cancelled)
    } else {
        outcome
    };

    let c = console.borrow();
    let (console_text, dropped) = (c.buf.clone(), c.dropped);
    drop(c);
    // The runtime, the context and every host binding die here, before the caller is answered.
    drop(rt);

    outcome.map(|value| Run {
        console: console_text,
        console_bytes_dropped: dropped,
        ops: ops.load(Ordering::Relaxed),
        ms: started.elapsed().as_millis() as u64,
        value: Some(value).filter(|v| !v.is_null()),
    })
}

/// The console buffer. Capped in bytes; the overflow is COUNTED, never silently lost.
struct Console {
    buf: String,
    dropped: usize,
    cap: usize,
    sink: Arc<dyn bough_plugin_js::ConsoleSink>,
}

impl Console {
    fn write(&mut self, line: &str) {
        self.sink.write(line);
        let room = self.cap.saturating_sub(self.buf.len());
        let add = line.len() + 1;
        if add <= room {
            self.buf.push_str(line);
            self.buf.push('\n');
        } else {
            self.dropped += add;
        }
    }
}

/// One bridged call: JSON in, JSON out, executed on the caller's runtime.
async fn call_host(
    table: Arc<std::collections::HashMap<String, Arc<dyn bough_plugin_js::HostCall>>>,
    name: String,
    args_json: String,
    rt: tokio::runtime::Handle,
) -> String {
    let Some(body) = table.get(&name).cloned() else {
        return refusal_json(RefusalKind::NotFound, &format!("no host function `{name}`"));
    };
    let args: Vec<serde_json::Value> = serde_json::from_str(&args_json).unwrap_or_default();
    let joined = rt.spawn(async move { body.call(args).await }).await;
    match joined {
        Err(e) => refusal_json(
            RefusalKind::Error,
            &format!("host call `{name}` panicked: {e}"),
        ),
        Ok(Err(r)) => refusal_json(r.kind, &r.message),
        Ok(Ok(v)) => serde_json::json!({ "ok": v }).to_string(),
    }
}

fn refusal_json(kind: RefusalKind, message: &str) -> String {
    serde_json::json!({
        "err": { "kind": kind, "message": message }
    })
    .to_string()
}

/// The prelude: `console`, and the namespace tree over `__host`. No capability of its own.
const PRELUDE: &str = r#"
globalThis.__fmt = (a) => {
  if (typeof a === 'string') return a;
  if (a instanceof Error) return String(a && a.stack ? a.stack : a);
  try { const s = JSON.stringify(a); return s === undefined ? String(a) : s; }
  catch (e) { return String(a); }
};
globalThis.console = {
  log:   (...a) => __log(a.map(__fmt).join(' ')),
  info:  (...a) => __log(a.map(__fmt).join(' ')),
  warn:  (...a) => __log(a.map(__fmt).join(' ')),
  error: (...a) => __log(a.map(__fmt).join(' ')),
  debug: (...a) => __log(a.map(__fmt).join(' ')),
};
globalThis.__bind = (names) => {
  const mk = (name) => async (...args) => {
    const raw = await __host(name, JSON.stringify(args.map(a => a === undefined ? null : a)));
    const out = JSON.parse(raw);
    if (out.err) {
      const e = new Error(out.err.message);
      e.kind = out.err.kind;
      throw e;
    }
    return out.ok;
  };
  for (const name of names) {
    const path = name.split('.');
    let holder = globalThis;
    for (let i = 0; i < path.length - 1; i++) {
      const seg = path[i];
      if (holder[seg] === undefined || holder[seg] === null) holder[seg] = {};
      holder = holder[seg];
    }
    const leaf = path[path.length - 1];
    const fn = mk(name);
    // A name that is BOTH a function and a namespace root (`bg`, `bg.output`) keeps whatever
    // properties an earlier name already hung on it.
    const prior = holder[leaf];
    if (prior && typeof prior === 'object') {
      for (const k of Object.keys(prior)) fn[k] = prior[k];
    }
    holder[leaf] = fn;
  }
};
"#;

fn to_json<'a>(ctx: &rquickjs::Ctx<'a>, v: Value<'a>) -> serde_json::Value {
    if v.is_undefined() || v.is_null() {
        return serde_json::Value::Null;
    }
    let stringify: Result<Function, _> = ctx
        .globals()
        .get::<_, rquickjs::Object>("JSON")
        .and_then(|j| j.get("stringify"));
    let Ok(stringify) = stringify else {
        return serde_json::Value::Null;
    };
    match stringify.call::<_, Option<String>>((v,)) {
        Ok(Some(s)) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
        _ => serde_json::Value::Null,
    }
}

fn thrown(e: impl std::fmt::Display) -> JsError {
    JsError::Thrown {
        message: e.to_string(),
        stack: None,
    }
}

/// Map a caught JS error onto the model's taxonomy. Memory and stack are QuickJS's OWN
/// exceptions — they arrive as InternalErrors with a known wording, not as a Rust error.
fn thrown_js(e: &CaughtError<'_>) -> JsError {
    let (message, stack) = describe(e);
    let low = message.to_lowercase();
    if low.contains("out of memory") || low.contains("null function pointer") {
        return JsError::MemoryExceeded { bytes: 0 };
    }
    if low.contains("stack overflow") || low.contains("maximum call stack") {
        return JsError::StackExceeded;
    }
    JsError::Thrown { message, stack }
}

fn syntax(e: &CaughtError<'_>, src: &str, bound: &[String]) -> JsError {
    let (message, _) = describe(e);
    JsError::Syntax {
        message: preflight::syntax_error_message(&message, src, bound),
        line: preflight::unterminated_string(src).map(|h| h.line as u32),
        col: preflight::unterminated_string(src).map(|h| h.col as u32),
    }
}

/// The program's own `eval` compiles the async IIFE and returns its promise: the body does not
/// begin until the promise is polled, so ANY exception out of that eval is a compile failure.
/// (A Rust-side error is the binding layer's, not the program's.)
fn syntax_or_thrown(e: &CaughtError<'_>, src: &str, bound: &[String]) -> JsError {
    match e {
        CaughtError::Error(_) => thrown_js(e),
        _ => syntax(e, src, bound),
    }
}

fn describe(e: &CaughtError<'_>) -> (String, Option<String>) {
    match e {
        CaughtError::Exception(x) => (x.message().unwrap_or_else(|| x.to_string()), x.stack()),
        CaughtError::Value(v) => (format!("{v:?}"), None),
        CaughtError::Error(err) => (err.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_js::{Caps, ConsoleSink, HostCall, HostFn, HostRefusal};
    use parking_lot::Mutex;
    use tokio_util::sync::CancellationToken;

    /// A sink that keeps every line, so a test can assert on what the CONSUMER would have
    /// stepped as well as on what `Run::console` holds.
    #[derive(Default)]
    struct Lines(Mutex<Vec<String>>);

    impl ConsoleSink for Lines {
        fn write(&self, line: &str) {
            self.0.lock().push(line.to_string());
        }
    }

    struct Echo;

    #[async_trait::async_trait]
    impl HostCall for Echo {
        async fn call(
            &self,
            args: Vec<serde_json::Value>,
        ) -> Result<serde_json::Value, HostRefusal> {
            Ok(serde_json::json!({ "got": args }))
        }
    }

    struct Refuse;

    #[async_trait::async_trait]
    impl HostCall for Refuse {
        async fn call(
            &self,
            _args: Vec<serde_json::Value>,
        ) -> Result<serde_json::Value, HostRefusal> {
            Err(HostRefusal {
                kind: RefusalKind::Denied,
                message: "the lane denies bash".into(),
            })
        }
    }

    fn caps() -> Caps {
        Caps {
            ops: 2_000_000,
            memory_bytes: 32 << 20,
            stack_bytes: 512 << 10,
            wall_ms: 5_000,
            console_bytes: 64 << 10,
        }
    }

    fn engine() -> QuickJsEngine {
        QuickJsEngine::new(Arc::new(QuickJsConfig {
            interrupt_check_ops: 1_000,
            max_concurrent_programs: 4,
        }))
    }

    fn program(
        src: &str,
        caps: Caps,
        host: Vec<HostFn>,
    ) -> (Program, Arc<Lines>, CancellationToken) {
        let sink = Arc::new(Lines::default());
        let cancel = CancellationToken::new();
        (
            Program {
                source: src.to_string(),
                caps,
                host,
                console: sink.clone(),
                cancel: cancel.clone(),
            },
            sink,
            cancel,
        )
    }

    fn host(name: &str, body: Arc<dyn HostCall>) -> HostFn {
        HostFn {
            name: name.to_string(),
            arity: 2,
            body,
        }
    }

    async fn run(src: &str) -> Result<Run, JsError> {
        let (p, _s, _c) = program(src, caps(), vec![]);
        engine().run(p).await
    }

    #[tokio::test]
    async fn hello_runs_and_returns_its_console() {
        invariant::clear();
        let (p, sink, _c) = program("console.log('hello', 1 + 1);", caps(), vec![]);
        let out = engine().run(p).await.expect("hello runs");
        assert_eq!(out.console, "hello 2\n");
        assert_eq!(sink.0.lock().clone(), vec!["hello 2".to_string()]);
        assert!(out.ops > 0, "the interrupt handler counted nothing");
    }

    #[tokio::test]
    async fn top_level_await_and_a_host_call_work() {
        let (p, _s, _c) = program(
            "const r = await bash('ls', ['tag']); console.log(JSON.stringify(r));",
            caps(),
            vec![host("bash", Arc::new(Echo))],
        );
        let out = engine().run(p).await.expect("the program runs");
        assert!(out.console.contains("\"ls\""), "{}", out.console);
        assert!(out.console.contains("\"tag\""), "{}", out.console);
    }

    #[tokio::test]
    async fn a_dotted_name_becomes_a_namespace() {
        let (p, _s, _c) = program(
            "console.log(typeof ledger, typeof ledger.search, typeof bg, typeof bg.output);",
            caps(),
            vec![
                host("bg", Arc::new(Echo)),
                host("bg.output", Arc::new(Echo)),
                host("ledger.search", Arc::new(Echo)),
            ],
        );
        let out = engine().run(p).await.expect("the program runs");
        assert_eq!(out.console, "object function function function\n");
    }

    #[tokio::test]
    async fn a_host_rejection_is_a_js_error_carrying_kind() {
        let (p, _s, _c) = program(
            "try { await bash('rm -rf /'); } catch (e) { console.log(e.kind, '|', e.message); }",
            caps(),
            vec![host("bash", Arc::new(Refuse))],
        );
        let out = engine().run(p).await.expect("the program handles it");
        assert_eq!(out.console, "denied | the lane denies bash\n");
    }

    #[tokio::test]
    async fn calling_a_name_that_is_not_bound_is_not_found() {
        // The binding table is the seam's; a name outside it cannot be reached at all.
        let out = run("console.log(typeof nosuch);").await.expect("runs");
        assert_eq!(out.console, "undefined\n");
    }

    #[tokio::test]
    async fn an_infinite_loop_hits_the_ops_cap() {
        let mut c = caps();
        c.ops = 50_000;
        c.wall_ms = 30_000;
        let (p, _s, _c) = program("while (true) {}", c, vec![]);
        assert_eq!(
            engine().run(p).await,
            Err(JsError::OpsExceeded { ops: 50_000 })
        );
    }

    #[tokio::test]
    async fn a_huge_allocation_hits_the_memory_cap() {
        let mut c = caps();
        c.memory_bytes = 4 << 20;
        let (p, _s, _c) = program(
            "const a = new Array(1e9).fill(0); console.log(a.length);",
            c,
            vec![],
        );
        assert_eq!(
            engine().run(p).await,
            Err(JsError::MemoryExceeded { bytes: 0 })
        );
    }

    #[tokio::test]
    async fn a_busy_loop_past_wall_ms_hits_the_time_cap() {
        let mut c = caps();
        c.wall_ms = 100;
        c.ops = u64::MAX;
        let (p, _s, _c) = program("while (true) {}", c, vec![]);
        let started = std::time::Instant::now();
        assert_eq!(
            engine().run(p).await,
            Err(JsError::TimeExceeded { ms: 100 })
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the cap did not bite"
        );
    }

    #[tokio::test]
    async fn deep_recursion_hits_the_stack_cap() {
        let mut c = caps();
        c.stack_bytes = 128 << 10;
        let (p, _s, _c) = program("function f(n) { return f(n + 1); } f(0);", c, vec![]);
        assert_eq!(engine().run(p).await, Err(JsError::StackExceeded));
    }

    #[tokio::test]
    async fn cancel_mid_program_yields_cancelled_and_no_run() {
        let mut c = caps();
        c.ops = u64::MAX;
        c.wall_ms = 30_000;
        let (p, _s, cancel) = program("while (true) {}", c, vec![]);
        let e = engine();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });
        assert_eq!(e.run(p).await, Err(JsError::Cancelled));
    }

    #[tokio::test]
    async fn cancel_while_parked_in_a_host_call_still_lands() {
        struct Slow;
        #[async_trait::async_trait]
        impl HostCall for Slow {
            async fn call(
                &self,
                _a: Vec<serde_json::Value>,
            ) -> Result<serde_json::Value, HostRefusal> {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(serde_json::Value::Null)
            }
        }
        let mut c = caps();
        c.wall_ms = 30_000;
        let (p, _s, cancel) = program(
            "await bash('sleep 30');",
            c,
            vec![host("bash", Arc::new(Slow))],
        );
        let e = engine();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });
        assert_eq!(e.run(p).await, Err(JsError::Cancelled));
    }

    #[tokio::test]
    async fn the_ambient_world_is_empty() {
        let out = run(
            "console.log(typeof fetch, typeof require, typeof process, typeof Deno, \
             typeof globalThis.std, typeof globalThis.os, typeof setTimeout, typeof Worker);",
        )
        .await
        .expect("runs");
        assert_eq!(
            out.console,
            "undefined undefined undefined undefined undefined undefined undefined undefined\n",
            "an ambient capability leaked into the sandbox"
        );
    }

    #[tokio::test]
    async fn importing_a_module_rejects() {
        let out = run("try { await import('fs'); console.log('LOADED'); } \
             catch (e) { console.log('refused'); }")
        .await
        .expect("the program handles it");
        assert_eq!(out.console, "refused\n");
    }

    #[tokio::test]
    async fn a_thrown_error_is_reported_as_thrown() {
        match run("throw new Error('boom');").await {
            Err(JsError::Thrown { message, .. }) => assert!(message.contains("boom"), "{message}"),
            other => panic!("expected Thrown, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_syntax_error_carries_the_model_facing_message() {
        match run("const p = \"one\ntwo\";").await {
            Err(JsError::Syntax { message, line, .. }) => {
                assert!(message.contains("closed by a real newline"), "{message}");
                assert_eq!(line, Some(1));
            }
            other => panic!("expected Syntax, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn check_delegates_the_parse_to_the_engine_that_runs_it() {
        let e = engine();
        assert_eq!(e.check("const x = 1; await bash('ls');").await, Ok(()));
        match e.check("const p = \"one\ntwo\";").await {
            Err(JsError::Syntax { message, .. }) => {
                assert!(message.contains("does not parse"), "{message}")
            }
            other => panic!("expected Syntax, got {other:?}"),
        }
        // A check never RUNS the program: a program that would throw still checks clean.
        assert_eq!(e.check("throw new Error('boom')").await, Ok(()));
    }

    #[tokio::test]
    async fn console_is_capped_in_bytes_and_the_overflow_is_counted() {
        let mut c = caps();
        c.console_bytes = 64;
        let (p, sink, _c) = program(
            "for (let i = 0; i < 50; i++) console.log('x'.repeat(20));",
            c,
            vec![],
        );
        let out = engine().run(p).await.expect("runs");
        assert!(out.console.len() <= 64, "{}", out.console.len());
        assert!(out.console_bytes_dropped > 0, "the overflow was hidden");
        assert_eq!(sink.0.lock().len(), 50, "the SINK sees every line");
    }

    /// The live-runtime count is process-global, so the one test that reads it runs alone.
    static COUNTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn every_runtime_is_dropped() {
        let _alone = COUNTING.lock().await;
        let before = invariant::live();
        let _ = run("console.log(1);").await;
        let _ = run("while (true) {}").await;
        let _ = run("throw new Error('x');").await;
        let _ = engine().check("const x = 1;").await;
        assert_eq!(
            invariant::live(),
            before,
            "a QuickJS runtime outlived its program"
        );
    }

    /// V3, the closure proof, stated positively: the program enumerates its OWN global surface
    /// and every name on it must be a pure-computation builtin, the seam's three bridges, or a
    /// name the seam bound. Nothing on this list can open a file, a socket, an environment
    /// variable or a process — which is what makes `bash` (a bound name, hence the pipeline)
    /// the only way to run a command.
    #[tokio::test]
    async fn the_whole_global_surface_is_pure_builtins_plus_the_bound_names() {
        // Pure computation only. `eval` compiles source in the SAME closed world, so it adds no
        // capability; `performance`/`queueMicrotask` are clocks and microtasks, not I/O.
        const PURE: &[&str] = &[
            "AggregateError",
            "Array",
            "ArrayBuffer",
            "AsyncDisposableStack",
            "Atomics",
            "BigInt",
            "BigInt64Array",
            "BigUint64Array",
            "Boolean",
            "DOMException",
            "DataView",
            "Date",
            "DisposableStack",
            "Error",
            "EvalError",
            "FinalizationRegistry",
            "Float16Array",
            "Float32Array",
            "Float64Array",
            "Function",
            "Infinity",
            "Int16Array",
            "Int32Array",
            "Int8Array",
            "InternalError",
            "Iterator",
            "JSON",
            "Map",
            "Math",
            "NaN",
            "Number",
            "Object",
            "Promise",
            "Proxy",
            "RangeError",
            "ReferenceError",
            "Reflect",
            "RegExp",
            "Set",
            "SharedArrayBuffer",
            "String",
            "SuppressedError",
            "Symbol",
            "SyntaxError",
            "TypeError",
            "URIError",
            "Uint16Array",
            "Uint32Array",
            "Uint8Array",
            "Uint8ClampedArray",
            "WeakMap",
            "WeakRef",
            "WeakSet",
            "atob",
            "btoa",
            "console",
            "decodeURI",
            "decodeURIComponent",
            "encodeURI",
            "encodeURIComponent",
            "escape",
            "eval",
            "globalThis",
            "isFinite",
            "isNaN",
            "parseFloat",
            "parseInt",
            "performance",
            "queueMicrotask",
            "undefined",
            "unescape",
        ];
        // The seam's own bridges. `__host` dispatches through the binding table and refuses an
        // unbound name (asserted below), so it is not a capability either.
        const BRIDGES: &[&str] = &["__bind", "__fmt", "__host", "__log"];

        let (p, _s, _c) = program(
            "console.log(Object.getOwnPropertyNames(globalThis).sort().join('\\n'));",
            caps(),
            vec![host("view", Arc::new(Echo))],
        );
        let out = engine().run(p).await.expect("runs");
        let leaked: Vec<&str> = out
            .console
            .lines()
            .filter(|n| !PURE.contains(n) && !BRIDGES.contains(n) && *n != "view")
            .collect();
        assert!(
            leaked.is_empty(),
            "an unaccounted global is reachable from the sandbox: {leaked:?}"
        );
        // And the converse: a program whose scope has no shell tool has no shell at all.
        assert!(!out.console.lines().any(|n| n == "bash"));
    }

    /// The named escape hatches, one program, no metadata: file, network, env, process, module.
    #[tokio::test]
    async fn no_file_network_env_or_process_access_is_possible() {
        let out = run(
            "const names = ['fetch','XMLHttpRequest','WebSocket','require','module','exports',\
             'process','Deno','Bun','Buffer','__filename','__dirname','std','os','scriptArgs',\
             'print','open','read','readFile','write','writeFile','loadFile','setTimeout',\
             'setInterval','Worker','importScripts','navigator','localStorage'];\
             console.log(names.filter(n => globalThis[n] !== undefined).join(',') || 'none');\
             try { console.log(eval('typeof require')); } catch (e) { console.log('eval-threw'); }",
        )
        .await
        .expect("runs");
        assert_eq!(
            out.console, "none\nundefined\n",
            "a capability leaked into the sandbox"
        );
    }

    /// `__host` is a dispatcher over the binding table, not a door: a name the seam did not bind
    /// is refused with `not_found`, so reaching past the injected set buys nothing.
    #[tokio::test]
    async fn the_raw_bridge_refuses_a_name_the_seam_did_not_bind() {
        let (p, _s, _c) = program(
            "const raw = await __host('bash', JSON.stringify(['rm -rf /'])); console.log(raw);",
            caps(),
            vec![host("view", Arc::new(Echo))],
        );
        let out = engine().run(p).await.expect("runs");
        assert!(
            out.console.contains("not_found") && out.console.contains("no host function `bash`"),
            "{}",
            out.console
        );
    }
}
