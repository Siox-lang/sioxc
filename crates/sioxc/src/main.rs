//! `sioxc` process entry point.

mod driver;

fn main() -> std::process::ExitCode {
    driver::run()
}
