// Width-normalizing casts around the Win32 ABI, where being explicit about the
// integer width is the point.
#![allow(clippy::unnecessary_cast)]

use windows::Win32::{
    Media::{timeBeginPeriod, timeEndPeriod},
    System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS},
    UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
    },
};

/// Set process DPI awareness early at startup, before any windows are created.
///
/// Attempts Per-Monitor V2 (Windows 10 1703+), which gives the best behaviour
/// for multi-monitor setups with different DPI values. Falls back to
/// System-aware on older builds.
pub fn set_dpi_awareness() {
    // SAFETY: these are one-shot process-global calls with no preconditions
    // beyond "call before creating windows".
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err() {
            // Per-Monitor V2 unavailable — fall back to system-aware.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
        }
    }
}

pub struct MultimediaTimerResolution {
    period_ms: u32,
}

impl MultimediaTimerResolution {
    pub fn begin_1ms() -> Self {
        let period_ms = 1;
        // SAFETY: balanced by Drop calling timeEndPeriod with the same period.
        unsafe { timeBeginPeriod(period_ms) };
        Self { period_ms }
    }
}

impl Drop for MultimediaTimerResolution {
    fn drop(&mut self) {
        // SAFETY: balances a successful best-effort timeBeginPeriod request.
        unsafe { timeEndPeriod(self.period_ms) };
    }
}

pub fn install_crash_handler() {
    unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
        const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
        const EXCEPTION_ACCESS_VIOLATION: u32 = 0xC0000005;

        if info.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let record = unsafe { &*(*info).ExceptionRecord };
        if record.ExceptionCode.0 as u32 != EXCEPTION_ACCESS_VIOLATION {
            return EXCEPTION_CONTINUE_SEARCH;
        }

        // Flush the in-memory trace ring to this run's session log first (best
        // effort, never blocks on the ring lock) so the lines leading up to the
        // fault are on disk before we append the crash marker below.
        crate::logging::dump_to_session_log_crash_safe();

        {
            let addr = record.ExceptionAddress as usize;
            let rw = if record.NumberParameters >= 1 {
                match record.ExceptionInformation[0] {
                    0 => "READ",
                    1 => "WRITE",
                    8 => "DEP",
                    _ => "UNKNOWN",
                }
            } else {
                "?"
            };
            let target = if record.NumberParameters >= 2 {
                record.ExceptionInformation[1] as usize
            } else {
                0
            };

            // Diagnostic: capture a symbolized backtrace of the faulting
            // thread. For an access violation the stack is intact, so this
            // pinpoints the teardown frame that faulted.
            let backtrace = std::panic::catch_unwind(|| {
                format!("{}", std::backtrace::Backtrace::force_capture())
            })
            .unwrap_or_else(|_| "<backtrace capture failed>".to_string());

            let msg = format!(
                "CRASH: ACCESS_VIOLATION at 0x{addr:016X}\n\
                 Type: {rw}\n\
                 Target address: 0x{target:016X}\n\
                 \n\
                 This is a hardware exception (not a Rust panic).\n\
                 Check this run's session log for the buffered trace leading up \
                 to this crash.\n\
                 \n\
                 Backtrace:\n{backtrace}\n"
            );

            // Both files carry this run's id, so a fault in one of a dozen
            // concurrent instances cannot overwrite another's evidence.
            if let Some(path) = crate::logging::crash_log_path() {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, &msg);
            }

            use std::io::Write;
            if let Some(path) = crate::logging::session_log_path() {
                // `create(true)` is load-bearing, not belt-and-braces. Under the
                // old fixed `session.log` the file was always there from an
                // earlier run; a per-run name exists only if the flush above
                // just made it — and that flush returns early, before creating
                // anything, when the ring lock is contended. That is exactly the
                // case this handler exists for (a thread faulting mid-`push`),
                // so without `create` the crash marker would be dropped in
                // precisely the scenario it is needed.
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&path)
                {
                    let _ = writeln!(f, "\n=== CRASH ===\n{msg}");
                }
            }
        }

        EXCEPTION_CONTINUE_SEARCH
    }

    // SAFETY: handler follows the VEH calling convention and does not unwind.
    unsafe {
        AddVectoredExceptionHandler(1, Some(handler));
    }
}
