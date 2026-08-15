//! In-memory ring-buffer logging.
//!
//! Tracing used to be redirected onto stderr (fd 2) and written to
//! `session.log` unbuffered — one `WriteFile` syscall per line, on whatever
//! thread logged. That taxed the UI/seek hot path during scrub churn.
//!
//! Instead, [`flog!`] appends formatted lines to a bounded in-memory ring.
//! The ring is flushed to `%APPDATA%/FastPlay/session-<run-id>.log` on normal
//! exit, on panic, and from the vectored crash handler — so the trace leading
//! up to a hard crash (d3d11 access violation) is still captured, without
//! paying a syscall on every line. In debug builds each line is also echoed to
//! stderr so the console stays live during development.
//!
//! # Why the file name carries a run id
//!
//! Both files used to have fixed names, written with `File::create` — which
//! truncates. Every instance therefore clobbered every other instance's trace.
//! That is precisely the case where the trace matters most: the GDI exhaustion
//! that wedged a player only reproduced with a dozen instances running, and the
//! evidence for it had to be reconstructed from the Windows event log because
//! FastPlay's own logs had overwritten each other. Each run now owns its file.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Maximum number of buffered log lines. A long scrub session evicts the
/// oldest lines first; the most recent ~`RING_CAPACITY` are what matter for a
/// crash post-mortem.
const RING_CAPACITY: usize = 2048;

/// How long a log file survives before [`init`] sweeps it. One file per run
/// accumulates quickly when a dozen instances are opened at a time, and a
/// week is well past the point where a trace is still worth reading.
const LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// File name prefixes owned by this module. Retention only ever deletes files
/// matching one of these *and* the `.log` extension.
const LOG_PREFIXES: [&str; 2] = ["session-", "crash-"];

static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static RUN_ID: OnceLock<String> = OnceLock::new();

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

/// Fix this run's identity and sweep stale log files.
///
/// Call once at startup, before anything can panic or fault. Resolving the run
/// id here means neither the panic hook nor the vectored crash handler has to
/// allocate one while the process is already coming apart.
pub fn init() {
    let _ = run_id();
    prune_old_logs();
}

/// Identifier unique to this run: UTC start time plus PID, e.g.
/// `20260810T143002Z-12345`.
///
/// The PID is last so a launcher that knows the process it started can find
/// exactly that run's log with the glob `session-*-<pid>.log`. Matching the
/// newest file instead would race another instance exiting at the same moment.
/// The timestamp leads so the names sort chronologically and carry enough
/// context to line a log up against an incident.
pub fn run_id() -> &'static str {
    RUN_ID.get_or_init(|| {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        format!("{}-{}", format_utc_stamp(secs), std::process::id())
    })
}

/// `%APPDATA%/FastPlay`, or `None` when `APPDATA` is unset.
fn log_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("FastPlay"))
}

/// Path of this run's crash log. See [`run_id`] for the naming scheme.
pub fn crash_log_path() -> Option<PathBuf> {
    Some(log_dir()?.join(format!("crash-{}.log", run_id())))
}

/// Path of this run's session log. The vectored crash handler appends its
/// crash marker here after [`dump_to_session_log_crash_safe`] has flushed.
pub fn session_log_path() -> Option<PathBuf> {
    Some(log_dir()?.join(format!("session-{}.log", run_id())))
}

/// Delete `session-*.log` and `crash-*.log` files older than [`LOG_RETENTION`].
///
/// Every failure is ignored: a missing directory, an unreadable entry, a file
/// another instance holds open, a clock that reports a modification time in the
/// future. Log hygiene must never be able to take down the player, and a file
/// that survives a sweep is simply collected by the next one.
fn prune_old_logs() {
    let Some(dir) = log_dir() else {
        return;
    };
    prune_logs_in(&dir, SystemTime::now());
}

/// The sweep itself, with the directory and the current time injected.
///
/// Split out so the retention policy — which is the only code in this module
/// that *deletes* anything — can be tested against synthesized files in a temp
/// directory, without mutating `APPDATA` for the whole test process or having
/// to backdate modification times.
fn prune_logs_in(dir: &std::path::Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("log") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !LOG_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        // `duration_since` errors when `modified` is in the future, which
        // `unwrap_or_default` correctly reads as "age zero — keep".
        if now.duration_since(modified).unwrap_or_default() > LOG_RETENTION {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Render epoch seconds as `YYYYMMDDTHHMMSSZ`.
///
/// Days-to-civil conversion after Howard Hinnant's `civil_from_days`, shifted
/// to an era starting 0000-03-01 so leap days land at the end of the cycle.
/// Written out rather than pulling in a date crate or widening the `windows`
/// feature list for `GetSystemTime` — it is a filename, and this way it is a
/// pure function with tests.
fn format_utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let seconds_of_day = secs % 86_400;
    let (hour, minute, second) = (
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    );

    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Flush the buffered lines to this run's session log, truncating any prior
/// contents of that file.
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
    let Some(path) = session_log_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let guard = if crash_safe {
        RING.try_lock().ok()
    } else {
        RING.lock().ok()
    };
    let Some(ring) = guard else {
        return;
    };

    use std::io::Write;
    if let Ok(mut file) = std::fs::File::create(&path) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_the_unix_epoch() {
        assert_eq!(format_utc_stamp(0), "19700101T000000Z");
    }

    #[test]
    fn last_second_of_a_day_does_not_roll_over() {
        assert_eq!(format_utc_stamp(86_399), "19700101T235959Z");
        assert_eq!(format_utc_stamp(86_400), "19700102T000000Z");
    }

    #[test]
    fn known_timestamps_round_trip() {
        // Both are widely-cited fixed points, so a broken era shift shows up.
        assert_eq!(format_utc_stamp(946_684_800), "20000101T000000Z");
        assert_eq!(format_utc_stamp(1_000_000_000), "20010909T014640Z");
    }

    #[test]
    fn leap_day_is_not_skipped() {
        // 2024-02-29 exists; 2023 has no 29th of February to land on.
        assert_eq!(format_utc_stamp(1_709_164_800), "20240229T000000Z");
        assert_eq!(format_utc_stamp(1_709_164_800 - 86_400), "20240228T000000Z");
        assert_eq!(format_utc_stamp(1_709_164_800 + 86_400), "20240301T000000Z");
    }

    #[test]
    fn century_years_follow_the_gregorian_rule() {
        // 2000 was a leap year (divisible by 400); 1900 and 2100 are not.
        assert_eq!(format_utc_stamp(951_782_400), "20000229T000000Z");
        assert_eq!(format_utc_stamp(4_107_542_400), "21000301T000000Z");
        assert_eq!(format_utc_stamp(4_107_542_400 - 86_400), "21000228T000000Z");
    }

    #[test]
    fn stamp_is_lexicographically_sortable() {
        // Fixed width with a leading year is what lets the directory listing
        // double as a chronological one.
        let earlier = format_utc_stamp(1_000_000_000);
        let later = format_utc_stamp(1_700_000_000);
        assert!(earlier < later);
        assert_eq!(earlier.len(), later.len());
    }

    #[test]
    fn run_id_ends_with_the_pid_after_a_hyphen() {
        // `session-*-<pid>.log` is how the bench harness finds the log of the
        // process it launched, so the PID must be the final hyphen-delimited
        // field and the stamp must not introduce a hyphen of its own.
        let id = run_id();
        let (stamp, pid) = id.rsplit_once('-').expect("run id has a hyphen");
        assert_eq!(pid, std::process::id().to_string());
        assert!(!stamp.contains('-'), "stamp must not contain a hyphen");
        assert!(stamp.ends_with('Z'), "stamp is UTC: {stamp}");
    }

    #[test]
    fn run_id_is_stable_within_a_process() {
        assert_eq!(run_id(), run_id());
    }

    /// A fresh temp directory holding one file per name in `names`.
    fn dir_with_files(tag: &str, names: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fastplay_logging_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        for name in names {
            std::fs::write(dir.join(name), b"trace").expect("write temp file");
        }
        dir
    }

    fn exists(dir: &std::path::Path, name: &str) -> bool {
        dir.join(name).exists()
    }

    /// Every file the sweep must consider, in one fixture: both owned
    /// prefixes, the two non-log files FastPlay keeps in the same directory,
    /// and a `.log` that belongs to somebody else.
    const FIXTURE: [&str; 5] = [
        "session-20260810T120000Z-4242.log",
        "crash-20260810T120000Z-4242.log",
        "recent.tsv",
        "settings.txt",
        "ffmpeg.log",
    ];

    #[test]
    fn logs_within_the_retention_window_survive() {
        let dir = dir_with_files("keep", &FIXTURE);

        // The files were written moments ago, so "now" is well inside the week.
        prune_logs_in(&dir, SystemTime::now());

        for name in FIXTURE {
            assert!(exists(&dir, name), "{name} was deleted while still fresh");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_past_the_retention_window_are_swept() {
        let dir = dir_with_files("sweep", &FIXTURE);

        // Advancing `now` past the window is equivalent to backdating every
        // file, and does not depend on the filesystem honouring a set mtime.
        let later = SystemTime::now() + LOG_RETENTION + Duration::from_secs(60);
        prune_logs_in(&dir, later);

        assert!(
            !exists(&dir, "session-20260810T120000Z-4242.log"),
            "an expired session log survived the sweep"
        );
        assert!(
            !exists(&dir, "crash-20260810T120000Z-4242.log"),
            "an expired crash log survived the sweep"
        );

        // Retention is scoped by prefix *and* extension. Deleting app state
        // would lose the user's recent-file list and settings; deleting
        // `ffmpeg.log` would be reaching outside this module's own files.
        assert!(exists(&dir, "recent.tsv"), "recent.tsv must never be swept");
        assert!(
            exists(&dir, "settings.txt"),
            "settings.txt must never be swept"
        );
        assert!(
            exists(&dir, "ffmpeg.log"),
            "a .log this module does not own must never be swept"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_log_directory_is_not_an_error() {
        // First run on a new machine: `init` calls this before anything has
        // created `%APPDATA%\FastPlay`.
        let absent = std::env::temp_dir().join("fastplay_logging_definitely_absent_zzz");
        let _ = std::fs::remove_dir_all(&absent);
        prune_logs_in(&absent, SystemTime::now());
    }
}
