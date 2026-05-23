//! Tiny POSIX process-introspection helper. Used by `tad watch`'s
//! singleton pidfile check and `tad doctor`'s stale-pidfile check —
//! both need the same "is this PID a live process I can signal"
//! semantics, and having one canonical implementation means the
//! `EPERM` edge case (process exists but we don't own it) is
//! documented in one place.

/// True iff `pid` names a currently-live process. Implementation is
/// `kill(pid, 0)`, which returns 0 if the process exists and we have
/// permission to signal it. `ESRCH` (no such process) → false;
/// `EPERM` (process exists but isn't ours) → still alive, true.
pub(crate) fn pid_is_alive(pid: i32) -> bool {
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_alive_self() {
        assert!(pid_is_alive(std::process::id() as i32));
    }

    #[test]
    fn handles_definitely_dead_pid_without_panic() {
        // PID 1 is always live on Unix. We probe a deliberately wild
        // value to exercise the not-alive path; there's a small chance
        // an unlucky pid roll picks this value, so we only assert that
        // the call returns (doesn't panic).
        let _ = pid_is_alive(2_147_483_640);
    }
}
