use windows::Win32::{
    Media::{timeBeginPeriod, timeEndPeriod},
    System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS},
};

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

        // Flush the in-memory trace ring to session.log first (best effort,
        // never blocks on the ring lock) so the lines leading up to the fault
        // are on disk before we append the crash marker below.
        crate::logging::dump_to_session_log_crash_safe();

        if let Some(appdata) = std::env::var_os("APPDATA") {
            let dir = std::path::PathBuf::from(appdata).join("FastPlay");
            let _ = std::fs::create_dir_all(&dir);

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

            let msg = format!(
                "CRASH: ACCESS_VIOLATION at 0x{addr:016X}\n\
                 Type: {rw}\n\
                 Target address: 0x{target:016X}\n\
                 \n\
                 This is a hardware exception (not a Rust panic).\n\
                 Check session.log for the buffered trace leading up to this crash.\n"
            );
            let _ = std::fs::write(dir.join("crash.log"), &msg);

            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .append(true)
                .open(dir.join("session.log"))
            {
                let _ = writeln!(f, "\n=== CRASH ===\n{msg}");
            }
        }

        EXCEPTION_CONTINUE_SEARCH
    }

    // SAFETY: handler follows the VEH calling convention and does not unwind.
    unsafe {
        AddVectoredExceptionHandler(1, Some(handler));
    }
}
