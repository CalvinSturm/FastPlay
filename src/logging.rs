//! In-memory ring-buffer logging.
//!
//! Tracing used to be redirected onto stderr (fd 2) and written to
//! `session.log` unbuffered — one `WriteFile` syscall per line, on whatever
//! thread logged. That taxed the UI/seek hot path during scrub churn.
//!
//! Instead, [`flog!`] appends formatted lines to a bounded in-memory ring.
//! The ring is flushed to `%APPDATA%/FastPlay/session.log` on normal exit,
//! on panic, and from the vectored crash handler — so the trace leading up to
//! a hard crash (d3d11 access violation) is still captured, without paying a
//! syscall on every line. In debug builds each line is also echoed to stderr
//! so the console stays live during development.

use std::{collections::VecDeque, sync::Mutex};

/// Maximum number of buffered log lines. A long scrub session evicts the
/// oldest lines first; the most recent ~`RING_CAPACITY` are what matter for a
/// crash post-mortem.
const RING_CAPACITY: usize = 2048;

static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Append a preformatted line to the ring buffer. Prefer the [`flog!`] macro.
pub fn push(line: String) {
    #[cfg(debug_assertions)]
    eprintln!("{line}");

    if let Ok(mut ring) = RING.lock() {
        if ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(line);
    }
}

/// Flush the buffered lines to `session.log`, truncating any prior contents.
///
/// Used on normal exit and panic, where blocking briefly on the lock is fine
/// (it serializes against any worker thread still logging).
pub fn dump_to_session_log() {
    write_ring(false);
}

/// Crash-safe variant for the vectored exception handler: never blocks on the
/// ring lock (a thread faulting mid-`push` would otherwise deadlock the
/// handler). If the lock is contended the dump is skipped — best effort.
pub fn dump_to_session_log_crash_safe() {
    write_ring(true);
}

fn write_ring(crash_safe: bool) {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return;
    };
    let dir = std::path::PathBuf::from(appdata).join("FastPlay");
    let _ = std::fs::create_dir_all(&dir);

    let guard = if crash_safe {
        RING.try_lock().ok()
    } else {
        RING.lock().ok()
    };
    let Some(ring) = guard else {
        return;
    };

    use std::io::Write;
    if let Ok(mut file) = std::fs::File::create(dir.join("session.log")) {
        for line in ring.iter() {
            let _ = writeln!(file, "{line}");
        }
        let _ = file.flush();
    }
}

/// Append a formatted line to the in-memory log ring (see module docs).
/// Drop-in replacement for `eprintln!` — same format-string arguments.
#[macro_export]
macro_rules! flog {
    ($($arg:tt)*) => {
        $crate::logging::push(format!($($arg)*))
    };
}
