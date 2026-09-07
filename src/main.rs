//! `sioxc` process entry point.

mod driver;

/// Entry point for the `sioxc` binary. All behavior lives in the `driver`
/// module; this only forwards the process exit code.
fn main() -> std::process::ExitCode {
    driver::run()
}
