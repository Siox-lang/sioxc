//! `sioxc` process entry point.

mod driver;

/// Stack for the compiler thread. Lowering recurses along value chains, one
/// level per link: a thousand chained writes to one local overflowed the
/// main thread's 8 MiB and aborted with no diagnostic. Like rustc, compile on
/// a thread with room; only the pages actually touched are committed.
const COMPILER_STACK: usize = 1 << 30;

/// Entry point for the `sioxc` binary. All behavior lives in the `driver`
/// module; this only forwards the process exit code.
fn main() -> std::process::ExitCode {
    std::thread::Builder::new()
        .name("sioxc".into())
        .stack_size(COMPILER_STACK)
        .spawn(driver::run)
        .expect("start the compiler thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
