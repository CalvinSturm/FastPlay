//! Runs the PowerShell run-log resolution tests as part of `cargo test`.
//!
//! `bench/FastPlayLog.psm1` decides which `session-<utc-stamp>-<pid>.log`
//! belongs to a run the harness launched. Getting that wrong is quiet rather
//! than loud: Windows recycles PIDs, so a stale log from an earlier run can
//! satisfy a `session-*-<pid>.log` glob, and an HDR oracle asserting
//! `path=HdrPqOutput` against it would pass while testing nothing.
//!
//! `bench/test-log-resolution.ps1` covers that case deterministically — it
//! launches no processes and writes only to a temp directory — so it is cheap
//! enough to run with the normal suite instead of only before a release.

use std::path::PathBuf;
use std::process::Command;

/// Repo root, derived from this test's own location rather than the working
/// directory, so the test works under any runner.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// PowerShell 7 if present. The bench scripts target `pwsh`, not the built-in
/// `powershell.exe`.
fn pwsh_available() -> bool {
    Command::new("pwsh")
        .args(["-NoProfile", "-Command", "exit 0"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
fn bench_run_log_resolution_rejects_stale_pid_matches() {
    if !pwsh_available() {
        // Skip rather than fail: pwsh is a bench-harness prerequisite, not a
        // build one, and the Rust suite must stay runnable without it.
        eprintln!("skipping: pwsh not on PATH");
        return;
    }

    let script = repo_root().join("bench").join("test-log-resolution.ps1");
    assert!(script.is_file(), "missing {}", script.display());

    let output = Command::new("pwsh")
        .arg("-NoProfile")
        .arg("-File")
        .arg(&script)
        .output()
        .expect("failed to run pwsh");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "bench/test-log-resolution.ps1 failed ({}):\n{stdout}\n{stderr}",
        output.status
    );
    // Guard against a script that exits 0 without asserting anything.
    assert!(
        stdout.contains("PASS: all"),
        "script did not report a pass summary:\n{stdout}\n{stderr}"
    );
}
