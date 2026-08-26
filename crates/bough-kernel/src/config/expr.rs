//! Invariant: `!!expr` functions are PURE — no filesystem, no network, no clock (Decision D10).
//! A config expression that touches the filesystem would make `--dump-config` lie on a different
//! machine. `ExprEnv` carries its own variable map so a test never has to mutate the process
//! environment.
//!
//! Grammar (hand-rolled recursive descent, deliberately tiny):
//!
//! ```text
//! expr := or
//! or   := and ("or" and)*
//! and  := not ("and" not)*
//! not  := "not" not | cmp
//! cmp  := atom (("==" | "!=") atom)?
//! atom := STRING | NUMBER | "true" | "false" | "null" | call | "(" expr ")"
//! call := IDENT "(" [ expr ("," expr)* ] ")"
//! ```
//!
//! Functions: `env(NAME)`, `env_or(NAME, DEFAULT)`, `home_path(REL)`, `bough_path(REL)`,
//! `platform()` (`"macos"`/`"linux"`), `profile()`. Anything else is a parse error naming the
//! unknown function.

use std::collections::BTreeMap;

/// A field that is either a literal or a `!!expr` source string awaiting evaluation.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr<T> {
    Literal(T),
    Source(String),
}

impl<T: Default> Default for Expr<T> {
    fn default() -> Self {
        Expr::Literal(T::default())
    }
}

impl<T: FromExprValue + Clone> Expr<T> {
    /// Evaluate against `env`. A `Literal` is returned as-is.
    pub fn eval(&self, env: &ExprEnv) -> Result<T, ExprError> {
        match self {
            Expr::Literal(t) => Ok(t.clone()),
            Expr::Source(src) => T::from_expr_value(eval_str(src, env)?),
        }
    }
}

/// Everything an expression may observe. Pure by construction.
pub struct ExprEnv {
    profile: String,
    vars: BTreeMap<String, String>,
}

impl ExprEnv {
    /// Snapshot the process environment and record the profile name.
    pub fn new(profile: &str) -> Self {
        ExprEnv {
            profile: profile.to_owned(),
            vars: std::env::vars().collect(),
        }
    }
    /// An environment with NO variables at all. Tests build from here so a stray variable on the
    /// developer's machine cannot make an assertion pass.
    pub fn empty(profile: &str) -> Self {
        ExprEnv {
            profile: profile.to_owned(),
            vars: BTreeMap::new(),
        }
    }
    /// Set one variable without touching the process environment. For tests.
    pub fn with_var(mut self, k: &str, v: &str) -> Self {
        self.vars.insert(k.to_owned(), v.to_owned());
        self
    }
    /// Look one variable up in this environment's own map.
    pub fn var(&self, k: &str) -> Option<&str> {
        self.vars.get(k).map(String::as_str)
    }
    /// The profile name `profile()` returns.
    pub fn profile(&self) -> &str {
        &self.profile
    }
}

/// The four value kinds an expression evaluates to.
#[derive(Clone, Debug, PartialEq)]
pub enum ExprValue {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

/// How a typed field reads an [`ExprValue`].
pub trait FromExprValue: Sized {
    fn from_expr_value(v: ExprValue) -> Result<Self, ExprError>;
}

impl FromExprValue for bool {
    fn from_expr_value(v: ExprValue) -> Result<Self, ExprError> {
        match v {
            ExprValue::Bool(b) => Ok(b),
            other => Err(ExprError::Type {
                expr: String::new(),
                expected: "bool",
                got: other.kind(),
            }),
        }
    }
}

impl FromExprValue for String {
    fn from_expr_value(v: ExprValue) -> Result<Self, ExprError> {
        match v {
            ExprValue::Str(s) => Ok(s),
            other => Err(ExprError::Type {
                expr: String::new(),
                expected: "string",
                got: other.kind(),
            }),
        }
    }
}

/// Replace every `!!expr` node in a YAML tree with its evaluated literal.
pub fn evaluate_tree(v: &serde_yaml::Value, env: &ExprEnv) -> Result<serde_yaml::Value, ExprError> {
    use serde_yaml::Value as V;
    Ok(match v {
        V::Tagged(t) if t.tag == EXPR_TAG => {
            let src = match &t.value {
                V::String(s) => s.clone(),
                other => serde_yaml::to_string(other)
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            };
            expr_value_to_yaml(eval_str(&src, env)?)
        }
        V::Sequence(items) => V::Sequence(
            items
                .iter()
                .map(|i| evaluate_tree(i, env))
                .collect::<Result<_, _>>()?,
        ),
        V::Mapping(m) => {
            let mut out = serde_yaml::Mapping::new();
            for (k, val) in m {
                out.insert(k.clone(), evaluate_tree(val, env)?);
            }
            V::Mapping(out)
        }
        other => other.clone(),
    })
}

/// The YAML tag a config expression carries.
///
/// REQUIREMENTS §0.5 spells it `!!expr`. `serde_yaml` 0.9 resolves `!!`-shorthand tags itself and
/// DISCARDS unknown ones, so `!!expr foo` reaches serde as the plain string `foo` — an expression
/// would silently become a literal. [`normalize_expr_tags`] rewrites the documented `!!expr`
/// spelling to the local tag `!expr`, which serde_yaml preserves; both spellings are accepted and
/// mean the same thing.
pub const EXPR_TAG: &str = "expr";

/// Rewrite `!!expr` tags to `!expr` in a YAML source, leaving quoted scalars and comments alone.
pub fn normalize_expr_tags(yaml: &str) -> String {
    let mut out = String::with_capacity(yaml.len());
    // Walk by CHARACTER, not by byte: a multi-byte char (an em dash in a comment, say) must be
    // copied through intact, and `bytes[i] as char` would shred it into Latin-1 garbage.
    let mut i = 0usize;
    #[derive(PartialEq)]
    enum St {
        Plain,
        Single,
        Double,
        Comment,
    }
    let mut st = St::Plain;
    let next_char = |i: usize| -> char { yaml[i..].chars().next().expect("i is a char boundary") };
    while i < yaml.len() {
        let c = next_char(i);
        match st {
            St::Comment => {
                if c == '\n' {
                    st = St::Plain;
                }
                out.push(c);
                i += c.len_utf8();
            }
            St::Single => {
                out.push(c);
                i += c.len_utf8();
                if c == '\'' {
                    st = St::Plain;
                }
            }
            St::Double => {
                out.push(c);
                i += c.len_utf8();
                if c == '\\' && i < yaml.len() {
                    let e = next_char(i);
                    out.push(e);
                    i += e.len_utf8();
                } else if c == '"' {
                    st = St::Plain;
                }
            }
            St::Plain => {
                if c == '#' {
                    st = St::Comment;
                    out.push(c);
                    i += c.len_utf8();
                } else if c == '\'' {
                    st = St::Single;
                    out.push(c);
                    i += c.len_utf8();
                } else if c == '"' {
                    st = St::Double;
                    out.push(c);
                    i += c.len_utf8();
                } else if yaml[i..].starts_with("!!expr") {
                    out.push_str("!expr");
                    i += "!!expr".len();
                } else {
                    out.push(c);
                    i += c.len_utf8();
                }
            }
        }
    }
    out
}

impl ExprValue {
    /// The name this kind uses in a type error.
    pub fn kind(&self) -> &'static str {
        match self {
            ExprValue::Str(_) => "string",
            ExprValue::Num(_) => "number",
            ExprValue::Bool(_) => "bool",
            ExprValue::Null => "null",
        }
    }
}

fn expr_value_to_yaml(v: ExprValue) -> serde_yaml::Value {
    match v {
        ExprValue::Str(s) => serde_yaml::Value::String(s),
        ExprValue::Num(n) => serde_yaml::Value::Number(serde_yaml::Number::from(n)),
        ExprValue::Bool(b) => serde_yaml::Value::Bool(b),
        ExprValue::Null => serde_yaml::Value::Null,
    }
}

// ---------------------------------------------------------------------------
// tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Str(String),
    Num(f64),
    Ident(String),
    LParen,
    RParen,
    Comma,
    Eq,
    Ne,
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, ExprError> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let at = i;
        match c {
            '(' => {
                out.push((Tok::LParen, at));
                i += 1;
            }
            ')' => {
                out.push((Tok::RParen, at));
                i += 1;
            }
            ',' => {
                out.push((Tok::Comma, at));
                i += 1;
            }
            '=' | '!' => {
                if i + 1 < b.len() && b[i + 1] == b'=' {
                    out.push((if c == '=' { Tok::Eq } else { Tok::Ne }, at));
                    i += 2;
                } else {
                    return Err(ExprError::Parse {
                        expr: src.to_owned(),
                        token: c.to_string(),
                        at,
                    });
                }
            }
            '"' | '\'' => {
                let quote = c;
                i += 1;
                let mut s = String::new();
                loop {
                    if i >= b.len() {
                        return Err(ExprError::Parse {
                            expr: src.to_owned(),
                            token: "unterminated string".into(),
                            at,
                        });
                    }
                    let ch = b[i] as char;
                    if ch == '\\' && quote == '"' && i + 1 < b.len() {
                        s.push(b[i + 1] as char);
                        i += 2;
                        continue;
                    }
                    if ch == quote {
                        i += 1;
                        break;
                    }
                    s.push(ch);
                    i += 1;
                }
                out.push((Tok::Str(s), at));
            }
            _ if c.is_ascii_digit()
                || (c == '-' && i + 1 < b.len() && (b[i + 1] as char).is_ascii_digit()) =>
            {
                let start = i;
                i += 1;
                while i < b.len() && ((b[i] as char).is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                let text = &src[start..i];
                let n: f64 = text.parse().map_err(|_| ExprError::Parse {
                    expr: src.to_owned(),
                    token: text.to_owned(),
                    at: start,
                })?;
                out.push((Tok::Num(n), at));
            }
            _ if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < b.len() && ((b[i] as char).is_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push((Tok::Ident(src[start..i].to_owned()), at));
            }
            _ => {
                return Err(ExprError::Parse {
                    expr: src.to_owned(),
                    token: c.to_string(),
                    at,
                })
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// AST + recursive-descent parser
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Func {
    Env,
    EnvOr,
    HomePath,
    BoughPath,
    Platform,
    Profile,
}

impl Func {
    fn lookup(name: &str) -> Option<(Func, usize)> {
        Some(match name {
            "env" => (Func::Env, 1),
            "env_or" => (Func::EnvOr, 2),
            "home_path" => (Func::HomePath, 1),
            "bough_path" => (Func::BoughPath, 1),
            "platform" => (Func::Platform, 0),
            "profile" => (Func::Profile, 0),
            _ => return None,
        })
    }
    fn name(&self) -> &'static str {
        match self {
            Func::Env => "env",
            Func::EnvOr => "env_or",
            Func::HomePath => "home_path",
            Func::BoughPath => "bough_path",
            Func::Platform => "platform",
            Func::Profile => "profile",
        }
    }
}

#[derive(Clone, Debug)]
enum Node {
    Lit(ExprValue),
    Not(Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Cmp {
        lhs: Box<Node>,
        rhs: Box<Node>,
        negated: bool,
    },
    Call(Func, Vec<Node>),
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }
    fn at(&self) -> usize {
        self.toks
            .get(self.pos)
            .map(|(_, a)| *a)
            .unwrap_or(self.src.len())
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|(t, _)| t.clone());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn err(&self, token: impl Into<String>) -> ExprError {
        ExprError::Parse {
            expr: self.src.to_owned(),
            token: token.into(),
            at: self.at(),
        }
    }
    fn eat_ident(&mut self, kw: &str) -> bool {
        if let Some(Tok::Ident(i)) = self.peek() {
            if i == kw {
                self.pos += 1;
                return true;
            }
        }
        false
    }

    fn parse_or(&mut self) -> Result<Node, ExprError> {
        let mut lhs = self.parse_and()?;
        while self.eat_ident("or") {
            let rhs = self.parse_and()?;
            lhs = Node::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn parse_and(&mut self) -> Result<Node, ExprError> {
        let mut lhs = self.parse_not()?;
        while self.eat_ident("and") {
            let rhs = self.parse_not()?;
            lhs = Node::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn parse_not(&mut self) -> Result<Node, ExprError> {
        if self.eat_ident("not") {
            return Ok(Node::Not(Box::new(self.parse_not()?)));
        }
        self.parse_cmp()
    }
    fn parse_cmp(&mut self) -> Result<Node, ExprError> {
        let lhs = self.parse_atom()?;
        let negated = match self.peek() {
            Some(Tok::Eq) => false,
            Some(Tok::Ne) => true,
            _ => return Ok(lhs),
        };
        self.pos += 1;
        let rhs = self.parse_atom()?;
        Ok(Node::Cmp {
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            negated,
        })
    }
    fn parse_atom(&mut self) -> Result<Node, ExprError> {
        match self.bump() {
            Some(Tok::Str(s)) => Ok(Node::Lit(ExprValue::Str(s))),
            Some(Tok::Num(n)) => Ok(Node::Lit(ExprValue::Num(n))),
            Some(Tok::LParen) => {
                let inner = self.parse_or()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(inner),
                    _ => Err(self.err(")")),
                }
            }
            Some(Tok::Ident(name)) => match name.as_str() {
                "true" => Ok(Node::Lit(ExprValue::Bool(true))),
                "false" => Ok(Node::Lit(ExprValue::Bool(false))),
                "null" => Ok(Node::Lit(ExprValue::Null)),
                _ => {
                    let (f, arity) = Func::lookup(&name).ok_or(ExprError::UnknownFunction {
                        expr: self.src.to_owned(),
                        name: name.clone(),
                    })?;
                    if self.peek() != Some(&Tok::LParen) {
                        return Err(self.err("("));
                    }
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() != Some(&Tok::RParen) {
                        loop {
                            args.push(self.parse_or()?);
                            if self.peek() == Some(&Tok::Comma) {
                                self.pos += 1;
                                continue;
                            }
                            break;
                        }
                    }
                    match self.bump() {
                        Some(Tok::RParen) => {}
                        _ => return Err(self.err(")")),
                    }
                    if args.len() != arity {
                        return Err(ExprError::Arity {
                            expr: self.src.to_owned(),
                            name: f.name(),
                            expected: arity,
                            got: args.len(),
                        });
                    }
                    Ok(Node::Call(f, args))
                }
            },
            Some(t) => Err(self.err(format!("{t:?}"))),
            None => Err(self.err("end of expression")),
        }
    }
}

fn parse(src: &str) -> Result<Node, ExprError> {
    let toks = lex(src)?;
    let mut p = Parser { src, toks, pos: 0 };
    let node = p.parse_or()?;
    if p.pos != p.toks.len() {
        return Err(p.err(format!("{:?}", p.toks[p.pos].0)));
    }
    Ok(node)
}

/// Parse and evaluate one expression source.
pub fn eval_str(src: &str, env: &ExprEnv) -> Result<ExprValue, ExprError> {
    eval_node(&parse(src)?, src, env)
}

fn want_bool(v: ExprValue, src: &str) -> Result<bool, ExprError> {
    match v {
        ExprValue::Bool(b) => Ok(b),
        other => Err(ExprError::Type {
            expr: src.to_owned(),
            expected: "bool",
            got: other.kind(),
        }),
    }
}

fn want_str(v: ExprValue, src: &str) -> Result<String, ExprError> {
    match v {
        ExprValue::Str(s) => Ok(s),
        other => Err(ExprError::Type {
            expr: src.to_owned(),
            expected: "string",
            got: other.kind(),
        }),
    }
}

fn eval_node(n: &Node, src: &str, env: &ExprEnv) -> Result<ExprValue, ExprError> {
    Ok(match n {
        Node::Lit(v) => v.clone(),
        Node::Not(inner) => ExprValue::Bool(!want_bool(eval_node(inner, src, env)?, src)?),
        Node::And(a, b) => {
            if want_bool(eval_node(a, src, env)?, src)? {
                ExprValue::Bool(want_bool(eval_node(b, src, env)?, src)?)
            } else {
                ExprValue::Bool(false)
            }
        }
        Node::Or(a, b) => {
            if want_bool(eval_node(a, src, env)?, src)? {
                ExprValue::Bool(true)
            } else {
                ExprValue::Bool(want_bool(eval_node(b, src, env)?, src)?)
            }
        }
        Node::Cmp { lhs, rhs, negated } => {
            let eq = eval_node(lhs, src, env)? == eval_node(rhs, src, env)?;
            ExprValue::Bool(if *negated { !eq } else { eq })
        }
        Node::Call(f, args) => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval_node(a, src, env)?);
            }
            match f {
                Func::Env => {
                    let name = want_str(vals.remove(0), src)?;
                    match env.var(&name) {
                        Some(v) => ExprValue::Str(v.to_owned()),
                        None => {
                            return Err(ExprError::MissingEnv {
                                expr: src.to_owned(),
                                name,
                            })
                        }
                    }
                }
                Func::EnvOr => {
                    let name = want_str(vals.remove(0), src)?;
                    match env.var(&name) {
                        Some(v) => ExprValue::Str(v.to_owned()),
                        None => vals.remove(0),
                    }
                }
                Func::HomePath => {
                    let rel = want_str(vals.remove(0), src)?;
                    ExprValue::Str(join_abs(env.home_dir(), &rel))
                }
                Func::BoughPath => {
                    let rel = want_str(vals.remove(0), src)?;
                    ExprValue::Str(join_abs(env.bough_home(), &rel))
                }
                Func::Platform => ExprValue::Str(PLATFORM.to_owned()),
                Func::Profile => ExprValue::Str(env.profile.clone()),
            }
        }
    })
}

fn join_abs(base: std::path::PathBuf, rel: &str) -> String {
    let p = base.join(rel.trim_start_matches('/'));
    p.to_string_lossy().into_owned()
}

/// `"macos"` or `"linux"`; anything else reports its `std::env::consts::OS` name.
pub const PLATFORM: &str = std::env::consts::OS;

impl ExprEnv {
    /// `$HOME` as this environment sees it, falling back to the process's home directory.
    fn home_dir(&self) -> std::path::PathBuf {
        match self.vars.get("HOME") {
            Some(h) => std::path::PathBuf::from(h),
            None => bough_util::home_dir(),
        }
    }
    /// `$BOUGH_HOME`, else `$HOME/.bough`.
    fn bough_home(&self) -> std::path::PathBuf {
        match self.vars.get("BOUGH_HOME") {
            Some(h) => std::path::PathBuf::from(h),
            None => self.home_dir().join(".bough"),
        }
    }
}

impl<T: serde::Serialize> serde::Serialize for Expr<T> {
    /// A `Literal` serialises as its value; a `Source` serialises back to its `!!expr` tag.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Expr::Literal(t) => t.serialize(s),
            Expr::Source(src) => serde_yaml::value::TaggedValue {
                tag: serde_yaml::value::Tag::new(EXPR_TAG),
                value: serde_yaml::Value::String(src.clone()),
            }
            .serialize(s),
        }
    }
}

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Expr<T> {
    /// A `!!expr`-tagged scalar becomes `Source`; anything else is deserialized as `T`.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let v = serde_yaml::Value::deserialize(d)?;
        if let serde_yaml::Value::Tagged(t) = &v {
            if t.tag == EXPR_TAG {
                let src = match &t.value {
                    serde_yaml::Value::String(s) => s.clone(),
                    other => serde_yaml::to_string(other)
                        .map_err(D::Error::custom)?
                        .trim()
                        .to_owned(),
                };
                return Ok(Expr::Source(src));
            }
        }
        T::deserialize(v)
            .map(Expr::Literal)
            .map_err(D::Error::custom)
    }
}

/// Parse and evaluation failures. Every variant names the offending source text.
#[derive(Debug, thiserror::Error)]
pub enum ExprError {
    #[error("expression `{expr}`: unexpected `{token}` at byte {at}")]
    Parse {
        expr: String,
        token: String,
        at: usize,
    },
    #[error("expression `{expr}`: unknown function `{name}`")]
    UnknownFunction { expr: String, name: String },
    #[error("expression `{expr}`: `{name}` takes {expected} argument(s), got {got}")]
    Arity {
        expr: String,
        name: &'static str,
        expected: usize,
        got: usize,
    },
    #[error("expression `{expr}`: expected {expected}, got {got}")]
    Type {
        expr: String,
        expected: &'static str,
        got: &'static str,
    },
    #[error("expression `{expr}`: environment variable `{name}` is not set")]
    MissingEnv { expr: String, name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> ExprEnv {
        // `empty` deliberately: a variable set on the developer's machine must not decide a test.
        ExprEnv::empty("tui")
    }

    #[test]
    fn env_lookup() {
        let e = env().with_var("BOUGH_MODEL", "opus");
        assert_eq!(
            eval_str("env(\"BOUGH_MODEL\")", &e).unwrap(),
            ExprValue::Str("opus".into())
        );
        let err = eval_str("env(\"NOPE_NOT_SET\")", &e).unwrap_err();
        assert!(format!("{err}").contains("NOPE_NOT_SET"), "{err}");
    }

    #[test]
    fn env_or_default() {
        let e = env();
        assert_eq!(
            eval_str("env_or(\"NOPE\", \"fallback\")", &e).unwrap(),
            ExprValue::Str("fallback".into())
        );
        let e = e.with_var("NOPE", "set");
        assert_eq!(
            eval_str("env_or(\"NOPE\", \"fallback\")", &e).unwrap(),
            ExprValue::Str("set".into())
        );
    }

    #[test]
    fn home_path_is_absolute() {
        let e = env().with_var("HOME", "/Users/tester");
        let v = eval_str("home_path(\"notes\")", &e).unwrap();
        assert_eq!(v, ExprValue::Str("/Users/tester/notes".into()));
        assert!(std::path::Path::new("/Users/tester/notes").is_absolute());
        let e = e.with_var("BOUGH_HOME", "/Users/tester/.bough");
        assert_eq!(
            eval_str("bough_path(\"bough.db\")", &e).unwrap(),
            ExprValue::Str("/Users/tester/.bough/bough.db".into())
        );
    }

    #[test]
    fn platform_matches_the_host() {
        let want = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else {
            std::env::consts::OS
        };
        assert_eq!(
            eval_str("platform()", &env()).unwrap(),
            ExprValue::Str(want.into())
        );
        assert_eq!(
            eval_str("profile()", &env()).unwrap(),
            ExprValue::Str("tui".into())
        );
    }

    #[test]
    fn not_and_or_precedence() {
        let e = env();
        // `not` binds tightest, then `and`, then `or`.
        assert_eq!(
            eval_str("not false and true or false", &e).unwrap(),
            ExprValue::Bool(true)
        );
        assert_eq!(
            eval_str("false and true or false", &e).unwrap(),
            ExprValue::Bool(false)
        );
        assert_eq!(
            eval_str("true or false and false", &e).unwrap(),
            ExprValue::Bool(true)
        );
        // Parentheses override.
        assert_eq!(
            eval_str("not (true and true)", &e).unwrap(),
            ExprValue::Bool(false)
        );
    }

    #[test]
    fn equality_on_strings() {
        let e = env().with_var("LANE", "build");
        assert_eq!(
            eval_str("env(\"LANE\") == \"build\"", &e).unwrap(),
            ExprValue::Bool(true)
        );
        assert_eq!(
            eval_str("env(\"LANE\") != \"build\"", &e).unwrap(),
            ExprValue::Bool(false)
        );
        assert_eq!(
            eval_str("platform() == \"plan9\"", &e).unwrap(),
            ExprValue::Bool(false)
        );
    }

    #[test]
    fn unknown_function_is_a_parse_error() {
        let err = eval_str("read_file(\"/etc/passwd\")", &env()).unwrap_err();
        assert!(
            matches!(&err, ExprError::UnknownFunction { name, .. } if name == "read_file"),
            "{err}"
        );
        assert!(format!("{err}").contains("read_file"), "{err}");
    }

    #[test]
    fn expr_in_a_nested_config_value_is_evaluated() {
        let e = env().with_var("DB", "/var/db/bough.sqlite");
        let src = "
store:
  path: !!expr env(\"DB\")
  flags: [!!expr platform(), plain]
";
        let v: serde_yaml::Value = serde_yaml::from_str(&normalize_expr_tags(src)).unwrap();
        let out = evaluate_tree(&v, &e).unwrap();
        let store = out.get("store").unwrap();
        assert_eq!(
            store.get("path").unwrap().as_str().unwrap(),
            "/var/db/bough.sqlite"
        );
        let flags = store.get("flags").unwrap().as_sequence().unwrap();
        assert_eq!(flags[0].as_str().unwrap(), PLATFORM);
        assert_eq!(flags[1].as_str().unwrap(), "plain");
    }

    #[test]
    fn literal_bool_needs_no_expr() {
        #[derive(serde::Deserialize)]
        struct Row {
            disabled: Expr<bool>,
        }
        let r: Row = serde_yaml::from_str("disabled: true").unwrap();
        assert_eq!(r.disabled, Expr::Literal(true));
        assert!(r.disabled.eval(&env()).unwrap());

        let src = normalize_expr_tags("disabled: !!expr profile() == \"headless\"");
        let r: Row = serde_yaml::from_str(&src).unwrap();
        assert!(matches!(r.disabled, Expr::Source(_)));
        assert!(!r.disabled.eval(&env()).unwrap());
        assert!(r.disabled.eval(&ExprEnv::empty("headless")).unwrap());
    }

    #[test]
    fn expr_tags_inside_quoted_scalars_are_left_alone() {
        let out = normalize_expr_tags("a: \"!!expr not a tag\"\nb: !!expr platform()\n");
        assert!(out.contains("\"!!expr not a tag\""));
        assert!(out.contains("b: !expr platform()"));
    }

    #[test]
    fn non_ascii_survives_normalisation() {
        // A byte-wise walk turned the em dash into Latin-1 garbage and serde_yaml then rejected
        // the document with "control characters are not allowed".
        let src = "# the base bundle \u{2014} one row\nid: hello\nwhere: caf\u{e9}\n";
        let out = normalize_expr_tags(src);
        assert_eq!(out, src);
        serde_yaml::from_str::<serde_yaml::Value>(&out).expect("still valid YAML");
    }
}
