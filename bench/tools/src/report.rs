//! Invariant: the money column is arithmetic over `model-policy`'s price table, never an estimate.

use serde::{Deserialize, Serialize};

use crate::run::Arm;

/// One (task, arm) measurement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub task: String,
    #[serde(with = "arm_serde")]
    pub arm: Arm,
    pub passed: bool,
    /// Steps appended to the ledger for the task's wake.
    pub steps: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cents: Money,
}

/// Money in hundredths of a cent, so the table adds up without float drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Money(pub i64);

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "${:.4}", self.0 as f64 / 1_000_000.0)
    }
}

/// The whole run, ready to print and to paste into `docs/phase-codemode-plan.md` §8.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub rows: Vec<Row>,
}

impl Report {
    /// Pass rate, steps per task, tokens and $ per arm.
    ///
    /// WP-8 owns the body.
    pub fn render(&self) -> String {
        todo!("WP-8: the per-arm summary table")
    }
}

mod arm_serde {
    use super::Arm;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(a: &Arm, s: S) -> Result<S::Ok, S::Error> {
        match a {
            Arm::Typed => "typed",
            Arm::Codemode => "codemode",
        }
        .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arm, D::Error> {
        match String::deserialize(d)?.as_str() {
            "typed" => Ok(Arm::Typed),
            "codemode" => Ok(Arm::Codemode),
            other => Err(serde::de::Error::custom(format!("unknown arm `{other}`"))),
        }
    }
}
