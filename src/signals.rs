//! Ending cleanly when tilog is told to stop: a closed terminal (SIGHUP), `kill` (SIGTERM) or
//! SIGINT sent from outside (Ctrl+C itself is a key while the screen is drawn).
//!
//! Without this, such a signal ends the process on the spot and the `Drop` code that stops the
//! commands never runs. The commands sit in sessions of their own, so the terminal's hang-up
//! does not reach them either: a quiet `ssh … tail -F` would be left running.

use std::sync::atomic::{AtomicI32, Ordering};

/// The signal that arrived, or 0.
static RECEIVED: AtomicI32 = AtomicI32::new(0);

/// Setting a flag is all a signal handler may safely do; the main loop looks at it.
extern "C" fn on_signal(signal: libc::c_int) {
    RECEIVED.store(signal, Ordering::SeqCst);
}

/// From now on SIGHUP, SIGTERM and SIGINT ask the main loop to end instead of killing the
/// process, so everything is dropped and stopped the normal way.
pub fn install() {
    for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        // SAFETY: `on_signal` only stores to an atomic, which is async-signal-safe, and the
        // `sigaction` struct is zeroed (no flags, an empty mask) before the handler is set.
        #[allow(unsafe_code)]
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(signal, &raw const action, std::ptr::null_mut());
        }
    }
}

/// Whether one of the signals above has arrived.
pub fn stop_requested() -> bool {
    received().is_some()
}

/// The signal that arrived, if one did.
pub fn received() -> Option<i32> {
    Some(RECEIVED.load(Ordering::SeqCst)).filter(|&signal| signal != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hang_up_asks_to_stop_instead_of_killing_the_process() {
        install();
        // SAFETY: `raise` sends a signal to this process; the handler above only sets a flag.
        #[allow(unsafe_code)]
        unsafe {
            libc::raise(libc::SIGHUP);
        }
        assert!(stop_requested(), "still running, and asked to stop");
        assert_eq!(received(), Some(libc::SIGHUP));
    }
}
