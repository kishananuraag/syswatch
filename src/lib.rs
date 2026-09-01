//! Library entrypoint. Re-exports modules so `examples/bench_collector.rs`
//! (and any future integration tests) can drive the collector without the
//! TUI / CLI / service plumbing.

pub mod cli;
pub mod config;
pub mod logging;
pub mod service;
pub mod stats;
pub mod ui;