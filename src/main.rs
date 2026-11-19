mod cli;
mod logging;
mod config;
mod service;
mod stats;
mod storage;
mod ui;
#[cfg(target_os = "macos")]
mod machsample;
#[cfg(target_os = "macos")]
mod menubar;

fn main() {
    cli::run();
}
