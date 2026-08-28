//! Invariant: this bench compares two SURFACES over one task bank, and every pass predicate is
//! DATA — files' contents, steps appended, journal rows — never a model judgement. It is not in
//! `make gates`: it is a measurement, and Andrey decides on the numbers.

pub mod bank;
pub mod report;
pub mod run;

pub use bank::{bench_dir, Coverage, Pass, Task};
pub use report::{Money, Price, Report, Row, Summary};
pub use run::{Arm, Runner};
