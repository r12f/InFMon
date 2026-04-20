//! Tests for infmonctl signal handling (spec 007 §Signal handling).
//!
//! - SIGPIPE → exit 0 silently
//! - SIGINT  → exit 130
//! - SIGTERM → exit 143

#[cfg(unix)]
mod unix {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    fn infmonctl_bin() -> std::path::PathBuf {
        assert_cmd::cargo::cargo_bin("infmonctl")
    }

    /// SIGPIPE: spawn `infmonctl --help` piped to a reader that closes
    /// immediately. With SIG_IGN for SIGPIPE, writes return EPIPE instead
    /// of killing the process. Either way (output completes or hits EPIPE),
    /// the CLI must exit 0 per spec 007.
    ///
    /// NOTE: `--help` output is small enough to complete in a single write,
    /// so this mostly validates the exit-code contract. Once `log tail -f`
    /// produces real streaming output, a stronger SIGPIPE test should
    /// replace this one.
    #[test]
    fn sigpipe_exits_zero() {
        let mut child = Command::new(infmonctl_bin())
            .arg("--help")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn infmonctl");

        // Read a few bytes then close the pipe.
        let mut stdout = child.stdout.take().unwrap();
        let mut buf = [0u8; 4];
        let _ = std::io::Read::read(&mut stdout, &mut buf);
        drop(stdout);

        let status = child.wait().expect("wait for infmonctl");
        assert!(status.success(), "expected exit 0, got {:?}", status.code());
    }

    /// Helper: spawn `log tail --follow`, wait for readiness, send signal,
    /// and return the exit code.
    fn spawn_and_signal(sig: libc::c_int) -> i32 {
        let mut child = Command::new(infmonctl_bin())
            .args(["log", "tail", "--follow"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn infmonctl");

        // Wait for the process to be ready (prints to stderr).
        let stderr = child.stderr.take().unwrap();
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read stderr");
        assert!(
            line.contains("waiting for logs"),
            "expected readiness line, got: {line}"
        );
        drop(reader);

        // Send signal.
        unsafe {
            libc::kill(child.id() as i32, sig);
        }

        let status = child.wait().expect("wait for infmonctl");
        status.code().unwrap_or(-1)
    }

    /// SIGINT → exit 130.
    #[test]
    fn sigint_exits_130() {
        let code = spawn_and_signal(libc::SIGINT);
        assert_eq!(code, 130, "expected exit 130 (SIGINT), got {code}");
    }

    /// SIGTERM → exit 143.
    #[test]
    fn sigterm_exits_143() {
        let code = spawn_and_signal(libc::SIGTERM);
        assert_eq!(code, 143, "expected exit 143 (SIGTERM), got {code}");
    }
}
