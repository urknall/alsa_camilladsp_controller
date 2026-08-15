//! Benchmark plan generation, live measurement collectors, and reporting for
//! the piCoreDSP Gate 12 aloop-vs-ioplug benchmark comparison.
//!
//! Split across submodules to keep each concern focused:
//! - [`report`]: plan/report data structures, template generation, plan
//!   validation, and Markdown report rendering.
//! - [`runner`]: orchestrates a full automatic benchmark run across both
//!   backends.
//! - [`parsing`]: pure, allocation-light text parsers for `/proc` files and
//!   `aplay` output (unit-testable without any live system state).
//! - [`collectors`]: live system/CamillaDSP measurement collectors and
//!   environment auto-detection.
//! - [`measurement`]: per-backend measurement orchestration.

mod collectors;
mod measurement;
mod parsing;
mod report;
mod runner;

pub use report::{make_benchmark_plan_template, make_benchmark_report, validate_benchmark_plan};
pub use runner::{run_benchmark_both_backends, BenchmarkRunnerConfig};
