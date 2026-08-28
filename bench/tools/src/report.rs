//! Invariant: the money column is arithmetic over `model-policy`'s price table, never an estimate.
//! An UNKNOWN price is reported as unknown, never as zero (the `price.rs` rule this file reuses
//! rather than re-implements).

use serde::{Deserialize, Serialize};

use crate::run::Arm;

pub use bough_plugin_model_policy::price::{cost_usd, Price};

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
    /// `None` when the run reported no usage the bench could price (a live run through the
    /// subprocess boundary, where no usage footer exists yet — see `docs/codemode-merge-notes.md`).
    pub cents: Option<Money>,
    /// Why the row failed, when it did. Free text, for the eye only; never scored.
    #[serde(default)]
    pub note: Option<String>,
}

/// Money in MILLIONTHS of a dollar, so the table adds up without float drift.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Money(pub i64);

impl Money {
    /// Dollars → millionths, half-away-from-zero. The ONE crossing from the price table's `f64`.
    pub fn from_usd(usd: f64) -> Money {
        Money((usd * 1_000_000.0).round() as i64)
    }
    pub fn as_usd(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
}

impl std::ops::Add for Money {
    type Output = Money;
    fn add(self, o: Money) -> Money {
        Money(self.0 + o.0)
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "${:.4}", self.as_usd())
    }
}

/// The price of one round, from the table `model-policy` is configured with.
pub fn price_round(input_tokens: u64, output_tokens: u64, price: Option<&Price>) -> Option<Money> {
    let usage = bough_plugin_llm::Usage {
        input_tokens: input_tokens as i64,
        output_tokens: output_tokens as i64,
        reasoning_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
    };
    cost_usd(&usage, price).map(Money::from_usd)
}

/// The whole run, ready to print and to paste into `docs/phase-codemode-plan.md` §8.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub rows: Vec<Row>,
    /// `replay` or `live haiku`: the same table, said of two different providers.
    pub mode: String,
}

/// The per-arm aggregate the table prints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub tasks: usize,
    pub passed: usize,
    pub steps: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` if ANY row of the arm priced to unknown: a partial sum would read as a total.
    pub dollars: Option<Money>,
}

impl Summary {
    pub fn steps_per_task(&self) -> f64 {
        if self.tasks == 0 {
            0.0
        } else {
            self.steps as f64 / self.tasks as f64
        }
    }
    pub fn per_task(&self) -> Option<Money> {
        match (self.dollars, self.tasks) {
            (Some(d), n) if n > 0 => Some(Money(d.0 / n as i64)),
            _ => None,
        }
    }
}

impl Report {
    pub fn summary(&self, arm: Arm) -> Summary {
        let rows: Vec<&Row> = self.rows.iter().filter(|r| r.arm == arm).collect();
        let dollars = rows
            .iter()
            .try_fold(Money(0), |acc, r| r.cents.map(|c| acc + c));
        Summary {
            tasks: rows.len(),
            passed: rows.iter().filter(|r| r.passed).count(),
            steps: rows.iter().map(|r| r.steps).sum(),
            input_tokens: rows.iter().map(|r| r.input_tokens).sum(),
            output_tokens: rows.iter().map(|r| r.output_tokens).sum(),
            dollars,
        }
    }

    /// Pass rate, steps per task, tokens and $ per arm — the markdown §8 wants, verbatim.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("| arm | pass | steps/task | in tok | out tok | $ / bank | $ / task |\n");
        out.push_str("|---|---|---|---|---|---|---|\n");
        for arm in [Arm::Typed, Arm::Codemode] {
            let s = self.summary(arm);
            if s.tasks == 0 {
                continue;
            }
            out.push_str(&format!(
                "| {}, {} | {}/{} | {:.1} | {} | {} | {} | {} |\n",
                arm.label(),
                self.mode,
                s.passed,
                s.tasks,
                s.steps_per_task(),
                s.input_tokens,
                s.output_tokens,
                s.dollars.map(|d| d.to_string()).unwrap_or("—".into()),
                s.per_task().map(|d| d.to_string()).unwrap_or("—".into()),
            ));
        }
        out.push('\n');
        out.push_str("| task | arm | pass | steps | in | out | $ | note |\n");
        out.push_str("|---|---|---|---|---|---|---|---|\n");
        for r in &self.rows {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r.task,
                r.arm.label(),
                if r.passed { "yes" } else { "NO" },
                r.steps,
                r.input_tokens,
                r.output_tokens,
                r.cents.map(|d| d.to_string()).unwrap_or("—".into()),
                r.note.clone().unwrap_or_default(),
            ));
        }
        out
    }
}

mod arm_serde {
    use super::Arm;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(a: &Arm, s: S) -> Result<S::Ok, S::Error> {
        a.label().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arm, D::Error> {
        match String::deserialize(d)?.as_str() {
            "typed" => Ok(Arm::Typed),
            "codemode" => Ok(Arm::Codemode),
            other => Err(serde::de::Error::custom(format!("unknown arm `{other}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn haiku() -> Price {
        // bundles/bough-base.yml's `model.policy.prices` row for claude-haiku-4-5-20251001.
        Price {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_read_per_mtok: 0.1,
            cache_write_per_mtok: 1.25,
        }
    }

    /// The hand-computed case: 40,000 in at $1/MTok is $0.04; 2,000 out at $5/MTok is $0.01.
    #[test]
    fn the_dollar_arithmetic_matches_the_price_table_on_a_hand_computed_case() {
        let m = price_round(40_000, 2_000, Some(&haiku())).expect("a priced model");
        assert_eq!(m, Money(50_000), "$0.05 in millionths");
        assert_eq!(m.to_string(), "$0.0500");
    }

    #[test]
    fn an_unpriced_model_is_unknown_never_zero() {
        assert_eq!(price_round(1_000, 1_000, None), None);
    }

    #[test]
    fn an_arm_with_one_unpriced_row_has_no_total() {
        let row = |cents| Row {
            task: "t".into(),
            arm: Arm::Typed,
            passed: true,
            steps: 3,
            input_tokens: 10,
            output_tokens: 1,
            cents,
            note: None,
        };
        let r = Report {
            rows: vec![row(Some(Money(10))), row(None)],
            mode: "replay".into(),
        };
        assert_eq!(r.summary(Arm::Typed).dollars, None);
        assert_eq!(r.summary(Arm::Typed).steps, 6);
        assert!(r.render().contains("| typed, replay | 2/2 | 3.0 |"));
    }
}
