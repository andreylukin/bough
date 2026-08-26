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

impl<T: FromExprValue> Expr<T> {
    /// Evaluate against `env`. A `Literal` is returned as-is.
    pub fn eval(&self, env: &ExprEnv) -> Result<T, ExprError> {
        todo!("WP-4")
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
        todo!("WP-4")
    }
    /// Set one variable without touching the process environment. For tests.
    pub fn with_var(self, k: &str, v: &str) -> Self {
        todo!("WP-4")
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
        todo!("WP-4")
    }
}

impl FromExprValue for String {
    fn from_expr_value(v: ExprValue) -> Result<Self, ExprError> {
        todo!("WP-4")
    }
}

/// Replace every `!!expr` node in a YAML tree with its evaluated literal.
pub fn evaluate_tree(
    v: &serde_yaml::Value,
    env: &ExprEnv,
) -> Result<serde_yaml::Value, ExprError> {
    todo!("WP-4")
}

impl<T: serde::Serialize> serde::Serialize for Expr<T> {
    /// A `Literal` serialises as its value; a `Source` serialises back to its `!!expr` tag.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        todo!("WP-4")
    }
}

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Expr<T> {
    /// A `!!expr`-tagged scalar becomes `Source`; anything else is deserialized as `T`.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        todo!("WP-4")
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
