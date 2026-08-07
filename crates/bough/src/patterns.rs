//! `bough patterns [FILE]` — compress a log into the handful of statements it
//! is actually made of. Port of `src/cli/patterns.ts` (spec `specs/cli.md`).
//!
//! WHY IT IS `patterns` AND NOT `logs`. `bough logs` already exists and tails the
//! server's own log. "Two subcommands one letter apart, one of which reads
//! bough's log and the other of which reads yours, is a trap that would be
//! sprung mostly by people debugging something else at the time."
//!
//! Conventions are `cli/mcp.rs`'s:
//!
//!   - **Argument parsing is pure and total.** `parse_args` is a function over a
//!     string slice returning arguments or a usage error. It never reads the
//!     environment, never exits, never panics.
//!   - **Every effect is injected.** `run_patterns` takes its input source and
//!     two writers and RETURNS an exit code. `real_deps` is the only constructor
//!     that touches a real process.
//!
//! Exit codes:
//!
//!   0  the log was analyzed
//!   1  the input could not be read
//!   2  usage problem
//!
//! There is deliberately no "found errors" exit code. The command reports what
//! is in a file; whether an ERROR line is a failure is a question about the
//! caller's intent, and a non-zero exit would make `bough patterns` unusable in
//! the pipelines where it is most useful.

use std::io::{BufRead, BufReader, IsTerminal};

use bough_core::logs::analyze::{AnalyzeOptions, Analyzer};
use bough_core::logs::drain::DrainOptions;
use bough_core::logs::format::{to_human, to_json, to_llm};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Llm,
    Json,
    Human,
}

impl Format {
    fn flag(self) -> &'static str {
        match self {
            Format::Llm => "llm",
            Format::Json => "json",
            Format::Human => "human",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PatternArgs {
    /// The file to read, or `None` for stdin.
    pub file: Option<String>,
    pub format: Option<Format>,
    pub top: usize,
    pub colour: Option<bool>,
    /// Similarity threshold override for the clustering pass.
    pub threshold: Option<f64>,
    pub ref_year: Option<i64>,
}

impl Default for PatternArgs {
    fn default() -> Self {
        Self {
            file: None,
            format: None,
            top: 20,
            colour: None,
            threshold: None,
            ref_year: None,
        }
    }
}

/// `parseArgs`'s return: arguments, or a usage error. An EMPTY message is the
/// help sentinel — printed to stdout, exit 0.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Args(Box<PatternArgs>),
    UsageError(String),
}

pub const USAGE: &str = "usage: bough patterns [OPTIONS] [FILE]

  Compress a log into its distinct statements, with per-variable statistics,
  anomalies and correlations. Reads stdin when FILE is absent or is `-`.

  --llm             compact markdown for a model to read
  --json            structured output (the shape is stable)
  --human           colored terminal output
                    default: --human on a terminal, --llm otherwise

  --top N           patterns to show (default 20)
  --threshold F     clustering similarity, 0..1 (default 0.4). Raise it if
                    distinct statements are being merged; lower it if one
                    statement is splitting into near-duplicate patterns
  --year Y          year for timestamp formats that omit one, e.g. syslog
  --no-color        never emit ANSI, even on a terminal
  -h, --help        this

exit: 0 analyzed · 1 unreadable input · 2 usage";

/// Everything the command needs from the world.
pub trait PatternDeps {
    /// Yields the input's lines. `Err` when the source cannot be read — the
    /// message reaches the user verbatim.
    fn read_lines(
        &self,
        file: Option<&str>,
    ) -> Result<Box<dyn Iterator<Item = String> + '_>, String>;
    fn out(&self, line: &str);
    fn err(&self, line: &str);
    /// Whether stdout is a terminal, which picks the default format and colour.
    fn is_tty(&self) -> bool;
    /// Terminal width for the human view.
    fn width(&self) -> Option<usize> {
        None
    }
}

/// `Number(argv[++i])` in TS: a missing value must become a usage error rather
/// than a crash, and TS's `Number` accepts leading/trailing space and rejects
/// everything else as NaN.
fn number_at(argv: &[String], i: usize) -> Option<f64> {
    let raw = argv.get(i)?.trim();
    if raw.is_empty() {
        // `Number("")` is 0 in JS — which then fails every range check below,
        // exactly as it does here.
        return Some(0.0);
    }
    raw.parse::<f64>().ok()
}

/// Parse argv. Pure and total: every bad input becomes a `UsageError` string.
///
/// Flags may appear before or after the file, because people type them in both
/// orders and refusing one of them is a papercut with no upside.
pub fn parse_args(argv: &[String]) -> Parsed {
    let mut args = PatternArgs::default();
    let mut saw_file = false;

    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "-h" | "--help" => return Parsed::UsageError(String::new()),
            "--llm" | "--json" | "--human" => {
                let f = match a {
                    "--llm" => Format::Llm,
                    "--json" => Format::Json,
                    _ => Format::Human,
                };
                // An explicit second format is a contradiction, not a
                // refinement. Silently taking the last one produces output the
                // caller did not ask for and will parse with the wrong reader.
                if let Some(existing) = args.format {
                    if existing != f {
                        return Parsed::UsageError(format!(
                            "--{} and {a} cannot both be given",
                            existing.flag()
                        ));
                    }
                }
                args.format = Some(f);
            }
            "--no-color" | "--no-colour" => args.colour = Some(false),
            "--top" => {
                i += 1;
                let v = number_at(argv, i);
                match v {
                    Some(v) if v.fract() == 0.0 && v >= 1.0 && v.is_finite() => {
                        args.top = v as usize
                    }
                    _ => return Parsed::UsageError("--top needs a positive integer".to_string()),
                }
            }
            "--threshold" => {
                i += 1;
                match number_at(argv, i) {
                    Some(v) if v > 0.0 && v <= 1.0 => args.threshold = Some(v),
                    _ => {
                        return Parsed::UsageError(
                            "--threshold needs a number in (0,1]".to_string(),
                        )
                    }
                }
            }
            "--year" => {
                i += 1;
                match number_at(argv, i) {
                    Some(v) if v.fract() == 0.0 && (1970.0..=9999.0).contains(&v) => {
                        args.ref_year = Some(v as i64)
                    }
                    _ => return Parsed::UsageError("--year needs a four-digit year".to_string()),
                }
            }
            // The conventional spelling of "stdin, explicitly". Leaves `file`
            // unset.
            "-" => saw_file = true,
            _ => {
                if a.starts_with('-') {
                    return Parsed::UsageError(format!("unknown option {a}"));
                }
                if saw_file {
                    return Parsed::UsageError("only one FILE may be given".to_string());
                }
                saw_file = true;
                args.file = Some(a.to_string());
            }
        }
        i += 1;
    }
    Parsed::Args(Box::new(args))
}

/// Run the command. Returns an exit code; never exits the process itself.
pub fn run_patterns(argv: &[String], deps: &dyn PatternDeps) -> i32 {
    let parsed = match parse_args(argv) {
        Parsed::UsageError(message) => {
            if message.is_empty() {
                deps.out(USAGE);
                return 0;
            }
            deps.err(&format!("error: {message}"));
            deps.err(USAGE);
            return 2;
        }
        Parsed::Args(args) => *args,
    };

    // The default format follows the consumer, not a preference. On a terminal a
    // person is reading; off one, something else is, "and that something is far
    // more often a model or a script than a person running `less`".
    let format = parsed.format.unwrap_or(if deps.is_tty() {
        Format::Human
    } else {
        Format::Llm
    });
    let colour = parsed.colour.unwrap_or_else(|| deps.is_tty());

    // Lines are pushed through as they arrive and never collected. Buffering
    // them first is the obvious shape and it silently caps the tool at whatever
    // fits in memory, which would make every bounded sketch behind this
    // pointless.
    let mut analyzer = Analyzer::new(AnalyzeOptions {
        top: parsed.top,
        ref_year: parsed.ref_year,
        drain: DrainOptions {
            threshold: parsed
                .threshold
                .unwrap_or(DrainOptions::default().threshold),
            ..Default::default()
        },
    });
    match deps.read_lines(parsed.file.as_deref()) {
        Ok(lines) => {
            for line in lines {
                analyzer.push(&line);
            }
        }
        Err(message) => {
            deps.err(&format!(
                "error: cannot read {}: {message}",
                parsed.file.as_deref().unwrap_or("stdin")
            ));
            return 1;
        }
    }
    let analysis = analyzer.finish();

    // An empty input is not an error — an empty log file is a perfectly ordinary
    // thing to point this at, and a non-zero exit would break the pipelines that
    // do.
    if analysis.lines == 0 {
        deps.err("no log lines found");
        if format == Format::Json {
            // NOT stripped, unlike the rendered path below: the TS writes
            // `toJson(analysis)` verbatim here, so the empty-input document ends
            // with a blank line. Kept byte-for-byte — a consumer diffing the two
            // implementations would otherwise see the empty file as the one
            // route where they disagree.
            deps.out(&to_json(&analysis));
        }
        return 0;
    }

    let rendered = match format {
        Format::Json => to_json(&analysis),
        Format::Llm => to_llm(&analysis),
        Format::Human => to_human(&analysis, colour, deps.width().unwrap_or(80)),
    };
    // The single trailing newline is stripped; `out` supplies the line ending.
    deps.out(rendered.strip_suffix('\n').unwrap_or(&rendered));
    0
}

// ---------------------------------------------------------------------------
// The only impure constructor
// ---------------------------------------------------------------------------

pub struct RealDeps;

impl PatternDeps for RealDeps {
    /// Read a file or stdin as lines, without holding the whole input as one
    /// string.
    ///
    /// A file is streamed rather than read whole because the whole point of the
    /// tool is inputs too big to hold comfortably. `BufRead::lines` decodes
    /// incrementally, so a multi-byte character split across a read boundary
    /// survives, and it yields a final unterminated line.
    fn read_lines(
        &self,
        file: Option<&str>,
    ) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
        match file {
            None => {
                let reader = BufReader::new(std::io::stdin());
                Ok(Box::new(reader.lines().map_while(Result::ok)))
            }
            Some(path) => {
                // The open is where an unreadable input is caught; a read error
                // mid-stream ends the iteration rather than the process, which
                // is what the TS stream does too.
                let handle = std::fs::File::open(path).map_err(|e| e.to_string())?;
                let reader = BufReader::new(handle);
                Ok(Box::new(reader.lines().map_while(Result::ok)))
            }
        }
    }

    fn out(&self, line: &str) {
        println!("{line}");
    }

    fn err(&self, line: &str) {
        eprintln!("{line}");
    }

    fn is_tty(&self) -> bool {
        std::io::stdout().is_terminal()
    }

    fn width(&self) -> Option<usize> {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|w| *w > 0)
    }
}

#[cfg(test)]
mod tests {
    //! Port of `src/cli/patterns.test.ts`. "Every effect is injected, so these
    //! are ordinary function calls."
    use super::*;
    use std::cell::RefCell;

    struct Fixture {
        lines: Vec<String>,
        out: RefCell<Vec<String>>,
        err: RefCell<Vec<String>>,
        tty: bool,
    }

    impl Fixture {
        fn new(lines: Vec<String>, tty: bool) -> Self {
            Self {
                lines,
                out: RefCell::new(Vec::new()),
                err: RefCell::new(Vec::new()),
                tty,
            }
        }
        fn stdout(&self) -> String {
            self.out.borrow().join("\n")
        }
        fn stderr(&self) -> String {
            self.err.borrow().join("\n")
        }
    }

    impl PatternDeps for Fixture {
        fn read_lines(
            &self,
            file: Option<&str>,
        ) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            if file == Some("missing.log") {
                return Err("ENOENT".to_string());
            }
            Ok(Box::new(self.lines.clone().into_iter()))
        }
        fn out(&self, line: &str) {
            self.out.borrow_mut().push(line.to_string());
        }
        fn err(&self, line: &str) {
            self.err.borrow_mut().push(line.to_string());
        }
        fn is_tty(&self) -> bool {
            self.tty
        }
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// A small log with two statements, one of them failing.
    fn sample_log() -> Vec<String> {
        let mut lines = Vec::new();
        let base = 1_705_327_200_000i64; // 2024-01-15T14:00:00Z
        for i in 0..60i64 {
            lines.push(format!(
                "{} INFO Request from 10.0.1.{} completed in {}ms status=200",
                iso(base + i * 1000),
                i % 4,
                20 + (i % 30)
            ));
        }
        for i in 0..5i64 {
            lines.push(format!(
                "{} ERROR Timeout connecting to 10.0.9.{i} after {}ms",
                iso(base + i * 1000),
                5000 + i
            ));
        }
        lines
    }

    fn iso(ms: i64) -> String {
        bough_core::logs::format::iso_stamp(ms)
    }

    fn args_of(p: Parsed) -> PatternArgs {
        match p {
            Parsed::Args(a) => *a,
            Parsed::UsageError(e) => panic!("unexpected usage error: {e}"),
        }
    }

    fn is_usage_error(p: &Parsed) -> bool {
        matches!(p, Parsed::UsageError(_))
    }

    // -----------------------------------------------------------------------
    // Parsing — pure and total
    // -----------------------------------------------------------------------

    #[test]
    fn defaults_to_twenty_patterns_and_stdin() {
        let a = args_of(parse_args(&[]));
        assert_eq!(a.top, 20);
        assert_eq!(a.file, None);
        assert_eq!(a.format, None);
    }

    #[test]
    fn takes_flags_before_or_after_the_file() {
        let before = parse_args(&argv(&["--json", "--top", "5", "app.log"]));
        let after = parse_args(&argv(&["app.log", "--top", "5", "--json"]));
        assert_eq!(before, after);
        let a = args_of(before);
        assert_eq!(a.file.as_deref(), Some("app.log"));
        assert_eq!(a.format, Some(Format::Json));
        assert_eq!(a.top, 5);
    }

    #[test]
    fn treats_dash_as_stdin_rather_than_as_a_file() {
        assert_eq!(args_of(parse_args(&argv(&["-"]))).file, None);
    }

    #[test]
    fn rejects_two_contradicting_formats() {
        let a = parse_args(&argv(&["--json", "--llm"]));
        match a {
            Parsed::UsageError(e) => assert!(e.contains("cannot both be given"), "{e}"),
            _ => panic!("two formats were accepted"),
        }
    }

    #[test]
    fn accepts_a_format_repeated() {
        assert!(!is_usage_error(&parse_args(&argv(&["--json", "--json"]))));
    }

    #[test]
    fn validates_every_numeric_option() {
        for bad in [
            vec!["--top", "0"],
            vec!["--top", "x"],
            vec!["--threshold", "0"],
            vec!["--threshold", "1.5"],
            vec!["--year", "24"],
        ] {
            assert!(
                is_usage_error(&parse_args(&argv(&bad))),
                "{} was accepted",
                bad.join(" ")
            );
        }
        assert!(
            !is_usage_error(&parse_args(&argv(&["--threshold", "1"]))),
            "1.0 is a valid threshold"
        );
    }

    #[test]
    fn rejects_unknown_options_and_a_second_file() {
        assert!(is_usage_error(&parse_args(&argv(&["--nope"]))));
        assert!(is_usage_error(&parse_args(&argv(&["a.log", "b.log"]))));
    }

    #[test]
    fn never_panics_on_a_missing_option_value() {
        // Total by contract: a missing value must become a usage error rather
        // than a crash on `Number(undefined)`.
        for argvv in [
            vec!["--top"],
            vec!["--threshold"],
            vec!["--year"],
            vec!["--"],
        ] {
            let _ = parse_args(&argv(&argvv));
        }
    }

    // -----------------------------------------------------------------------
    // Running
    // -----------------------------------------------------------------------

    #[test]
    fn help_exits_zero_and_prints_usage() {
        let f = Fixture::new(vec![], false);
        assert_eq!(run_patterns(&argv(&["--help"]), &f), 0);
        assert!(f.stdout().contains("usage: bough patterns"));
    }

    #[test]
    fn a_usage_error_exits_2_and_explains_itself() {
        let f = Fixture::new(vec![], false);
        assert_eq!(run_patterns(&argv(&["--top", "0"]), &f), 2);
        assert!(f.stderr().contains("--top needs a positive integer"));
    }

    #[test]
    fn an_unreadable_input_exits_1() {
        let f = Fixture::new(vec![], false);
        assert_eq!(run_patterns(&argv(&["missing.log"]), &f), 1);
        assert!(f.stderr().contains("cannot read missing.log"));
    }

    #[test]
    fn an_empty_log_is_not_an_error() {
        let f = Fixture::new(vec![], false);
        assert_eq!(run_patterns(&[], &f), 0);
        assert!(f.stderr().contains("no log lines found"));
    }

    #[test]
    fn finding_errors_does_not_change_the_exit_code() {
        let f = Fixture::new(sample_log(), false);
        assert_eq!(run_patterns(&argv(&["--llm"]), &f), 0);
        assert!(f.stdout().contains("ERROR"));
    }

    // -----------------------------------------------------------------------
    // Format selection
    // -----------------------------------------------------------------------

    #[test]
    fn the_default_format_follows_the_consumer() {
        let piped = Fixture::new(sample_log(), false);
        run_patterns(&[], &piped);
        assert!(
            piped.stdout().starts_with("# 65 lines"),
            "piped output was not the llm format"
        );

        let tty = Fixture::new(sample_log(), true);
        run_patterns(&[], &tty);
        assert!(tty.stdout().contains("lines → "));
        assert!(tty.stdout().contains(" patterns"));
    }

    #[test]
    fn no_color_suppresses_ansi_on_a_terminal() {
        let plain = Fixture::new(sample_log(), true);
        run_patterns(&argv(&["--no-color"]), &plain);
        assert!(
            !plain.stdout().contains('\u{1b}'),
            "ANSI survived --no-color"
        );

        let coloured = Fixture::new(sample_log(), true);
        run_patterns(&[], &coloured);
        assert!(
            coloured.stdout().contains('\u{1b}'),
            "a terminal got no colour"
        );
    }

    #[test]
    fn json_emits_parseable_output_matching_the_analysis_shape() {
        let f = Fixture::new(sample_log(), false);
        assert_eq!(run_patterns(&argv(&["--json"]), &f), 0);
        let parsed: serde_json::Value = serde_json::from_str(&f.stdout()).unwrap();
        assert_eq!(parsed["lines"], 65);
        assert!(parsed["patternCount"].as_u64().unwrap() >= 2);
        assert!(parsed["patterns"].is_array());
        assert!(parsed["truncated"].is_boolean());
        let p = &parsed["patterns"][0];
        assert!(!p["template"].as_str().unwrap().is_empty());
        assert!(p["vars"].is_array());
    }

    #[test]
    fn json_on_an_empty_log_still_emits_an_object() {
        // A consumer parsing stdout must not have to special-case the empty file.
        let f = Fixture::new(vec![], false);
        run_patterns(&argv(&["--json"]), &f);
        let parsed: serde_json::Value = serde_json::from_str(&f.stdout()).unwrap();
        assert_eq!(parsed["lines"], 0);
    }

    // -----------------------------------------------------------------------
    // What the output says
    // -----------------------------------------------------------------------

    #[test]
    fn the_llm_view_leads_with_problems() {
        let f = Fixture::new(sample_log(), false);
        run_patterns(&argv(&["--llm"]), &f);
        let text = f.stdout();
        let problems = text.find("## Problems").expect("no problems section");
        let rest = text
            .find("## Everything else")
            .expect("no everything-else section");
        assert!(
            rest > problems,
            "the INFO pattern was rendered above the errors"
        );
    }

    #[test]
    fn no_output_format_advertises_anything() {
        for flag in ["--llm", "--human", "--json"] {
            let f = Fixture::new(sample_log(), flag == "--human");
            run_patterns(&argv(&[flag, "--no-color"]), &f);
            let lower = f.stdout().to_lowercase();
            for ad in ["powered by", "learn more", "http://", "https://"] {
                assert!(!lower.contains(ad), "{flag} carried a footer");
            }
        }
    }

    #[test]
    fn top_truncates_the_rendering_but_not_the_count() {
        let f = Fixture::new(sample_log(), false);
        run_patterns(&argv(&["--json", "--top", "1"]), &f);
        let parsed: serde_json::Value = serde_json::from_str(&f.stdout()).unwrap();
        assert_eq!(parsed["patterns"].as_array().unwrap().len(), 1);
        assert!(
            parsed["patternCount"].as_u64().unwrap() > 1,
            "patternCount was truncated along with the rendering"
        );
    }

    #[test]
    fn the_analysis_compresses_and_reports_its_own_reduction_honestly() {
        let f = Fixture::new(sample_log(), false);
        run_patterns(&argv(&["--json"]), &f);
        let parsed: serde_json::Value = serde_json::from_str(&f.stdout()).unwrap();
        let count = parsed["patternCount"].as_u64().unwrap();
        assert!(count <= 4, "65 lines produced {count} patterns");
        let totals: u64 = parsed["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["count"].as_u64().unwrap())
            .sum();
        assert_eq!(totals, 65, "counts do not add up to the lines read");
    }

    #[test]
    fn a_log_with_no_timestamps_analyzes_without_a_span() {
        let f = Fixture::new(
            vec![
                "make: entering dir /a/b".to_string(),
                "make: entering dir /a/c".to_string(),
                "cc -o x x.c".to_string(),
            ],
            false,
        );
        assert_eq!(run_patterns(&argv(&["--json"]), &f), 0);
        let parsed: serde_json::Value = serde_json::from_str(&f.stdout()).unwrap();
        assert!(parsed.get("timeSpan").is_none());
        assert_eq!(parsed["lines"], 3);
    }

    #[test]
    fn blank_lines_are_skipped_rather_than_clustered() {
        let f = Fixture::new(
            vec![
                "INFO a".to_string(),
                String::new(),
                "   ".to_string(),
                "INFO b".to_string(),
            ],
            false,
        );
        run_patterns(&argv(&["--json"]), &f);
        let parsed: serde_json::Value = serde_json::from_str(&f.stdout()).unwrap();
        assert_eq!(parsed["lines"], 2);
    }
}
