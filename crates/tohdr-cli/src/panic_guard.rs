//! Turns a `todo!()` panic from a half-built engine into a normal
//! [`anyhow::Error`] instead of an opaque backtrace.
//!
//! `tohdr-heif`, `tohdr-apple`, and `tohdr-portable` currently panic via
//! `todo!()` in most bodies. Letting that unwind to `main` prints Rust's
//! default panic banner, which is unreadable in a CLI and useless in
//! `--json` mode. [`catch`] converts it into a normal error, and temporarily
//! silences the default panic hook so nothing extra lands on stderr.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Run `f`, converting both an `Err` and a panic (e.g. `todo!()`) into one
/// [`anyhow::Error`]. `engine` and `op` name the call for the error message,
/// e.g. `("apple-imageio", "encode")`.
pub fn catch<T, E: std::fmt::Debug>(
    engine: &str,
    op: &str,
    f: impl FnOnce() -> Result<T, E>,
) -> anyhow::Result<T> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(prev_hook);

    match result {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(anyhow::anyhow!("{engine}: {op} failed: {e:?}")),
        Err(payload) => Err(anyhow::anyhow!(
            "{engine}: {op} is not yet implemented ({})",
            panic_message(&payload)
        )),
    }
}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_ok() {
        let r: anyhow::Result<u32> = catch("test-engine", "op", || Ok::<u32, String>(42));
        assert_eq!(r.unwrap(), 42);
    }

    #[test]
    fn reports_err_without_panicking() {
        let r: anyhow::Result<u32> = catch("test-engine", "op", || Err::<u32, String>("boom".into()));
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("test-engine"));
        assert!(msg.contains("op failed"));
        assert!(msg.contains("boom"));
    }

    #[test]
    fn converts_todo_panic_to_error() {
        let r: anyhow::Result<u32> = catch("test-engine", "op", || -> Result<u32, String> {
            todo!("not wired up yet")
        });
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("test-engine"));
        assert!(msg.contains("not yet implemented"));
        assert!(msg.contains("not wired up yet"));
    }
}
