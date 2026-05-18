//! Integration with the `cmux` CLI.
//!
//! When the TUI is running inside a cmux session, pressing Enter on a markdown
//! file should open it in a `[markdown]` surface to the right. cmux's own
//! `cmux markdown open` command does the rendering and live-reload for us; we
//! just need to keep track of the surface we opened so the next request can
//! close it first (so the new file replaces the old one instead of stacking
//! into a new pane).
//!
//! All side-effecting operations go through a [`CmuxClient`] trait so the app
//! state machine can be tested with a mock client.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use thiserror::Error;

/// Hard wall-clock cap on any single `cmux` subprocess call. cmux is a local
/// daemon and should respond essentially instantly — if it takes longer than
/// this, something is wrong (daemon stuck, socket starved, hung child). We
/// kill the child and surface a clear error rather than freezing the TUI.
const CMUX_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum CmuxError {
    #[error("cmux CLI not found on PATH")]
    NotFound,
    #[error("cmux is not currently running (socket unreachable)")]
    Unreachable,
    #[error("cmux command failed: {0}")]
    CommandFailed(String),
    #[error("could not parse cmux output: {0}")]
    ParseError(String),
    #[error("cmux command timed out after {0:?}")]
    Timeout(Duration),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Identifier for a surface created by `cmux markdown open`. The cmux CLI
/// returns these as e.g. `surface:429`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceId(pub String);

impl SurfaceId {
    pub fn as_arg(&self) -> &str {
        &self.0
    }
}

/// Successful result of `cmux markdown open`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenResult {
    pub surface: SurfaceId,
    pub pane: String,
    pub path: String,
}

/// Trait implemented by anything that can talk to cmux.
///
/// In production it is [`CliCmux`]; in tests we use [`MockCmux`].
pub trait CmuxClient: Send {
    fn is_available(&self) -> bool;
    fn open_markdown(&self, path: &std::path::Path) -> Result<OpenResult, CmuxError>;
    fn close_surface(&self, surface: &SurfaceId) -> Result<(), CmuxError>;
}

/// Default cmux client — shells out to the `cmux` CLI.
#[derive(Debug, Default)]
pub struct CliCmux {
    /// Override the binary path (mostly for tests).
    pub binary: Option<String>,
}

impl CliCmux {
    pub fn new() -> Self {
        Self::default()
    }

    fn bin(&self) -> &str {
        self.binary.as_deref().unwrap_or("cmux")
    }
}

impl CmuxClient for CliCmux {
    fn is_available(&self) -> bool {
        let mut cmd = Command::new(self.bin());
        cmd.arg("ping");
        run_with_timeout(cmd, CMUX_TIMEOUT)
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn open_markdown(&self, path: &std::path::Path) -> Result<OpenResult, CmuxError> {
        let mut cmd = Command::new(self.bin());
        cmd.arg("markdown").arg("open").arg(path);
        let out = run_with_timeout(cmd, CMUX_TIMEOUT)?;
        if !out.status.success() {
            return Err(CmuxError::CommandFailed(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        parse_open_output(&stdout)
    }

    fn close_surface(&self, surface: &SurfaceId) -> Result<(), CmuxError> {
        let mut cmd = Command::new(self.bin());
        cmd.arg("close-surface")
            .arg("--surface")
            .arg(surface.as_arg());
        let out = run_with_timeout(cmd, CMUX_TIMEOUT)?;
        if !out.status.success() {
            return Err(CmuxError::CommandFailed(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        Ok(())
    }
}

/// Spawn `cmd` and wait at most `timeout` for it to finish. If the child
/// outruns the budget we kill it and return [`CmuxError::Timeout`].
///
/// Background: a hung `cmux` daemon used to freeze the entire TUI because
/// `Command::output()` blocks until the child exits. With this wrapper the
/// worst case is a 5-second wait followed by a clean error overlay.
///
/// Implementation: stdout/stderr are drained on worker threads so we can't
/// deadlock on a full pipe buffer; the main thread polls `try_wait` with a
/// short sleep and kills the child if the deadline passes.
fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> Result<std::process::Output, CmuxError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = stdout.map(|mut s| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_reader = stderr.map(|mut s| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Don't bother joining the readers — pipes are closed
                    // now that the child is dead, so they'll return shortly.
                    return Err(CmuxError::Timeout(timeout));
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
    };

    let so = stdout_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let se = stderr_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout: so,
        stderr: se,
    })
}

/// Parse a line of the form:
///   `OK surface=surface:429 pane=pane:366 path=/some/file.md`
///
/// `path=` is parsed positionally — its value runs to the end of the line —
/// because filenames legitimately contain spaces. The earlier `surface=` and
/// `pane=` values are taken from their declared offsets, not by scanning the
/// whole line for `key=`, so a path that itself contains `surface=` can no
/// longer hijack the surface id.
pub fn parse_open_output(s: &str) -> Result<OpenResult, CmuxError> {
    let line = s
        .lines()
        .find(|l| l.trim_start().starts_with("OK"))
        .ok_or_else(|| CmuxError::ParseError(format!("no OK line in cmux output: {}", s.trim())))?;

    // surface= must be present.
    let surface_start = line
        .find(" surface=")
        .ok_or_else(|| CmuxError::ParseError(format!("missing surface=... in line: {}", line)))?
        + " surface=".len();
    let after_surface = &line[surface_start..];
    // surface value runs until the next known key (pane= or path=) or EOL.
    let surface_end = first_of(after_surface, &[" pane=", " path="]).unwrap_or(after_surface.len());
    let surface_val = after_surface[..surface_end].trim().to_string();

    // pane= and path= are optional. Look for them only after surface= so a
    // literal "pane=" inside the surface value isn't mis-parsed.
    let tail = &after_surface[surface_end..];
    let pane_val = if let Some(rest) = tail.strip_prefix(" pane=") {
        let end = rest.find(" path=").unwrap_or(rest.len());
        rest[..end].trim().to_string()
    } else {
        String::new()
    };
    // path= value extends to the end of the line.
    let path_val = tail
        .find(" path=")
        .map(|idx| tail[idx + " path=".len()..].trim_end().to_string())
        .unwrap_or_default();

    Ok(OpenResult {
        surface: SurfaceId(surface_val),
        pane: pane_val,
        path: path_val,
    })
}

/// Return the byte index of the leftmost occurrence of any of `needles`
/// within `hay`, or `None` if none matches.
fn first_of(hay: &str, needles: &[&str]) -> Option<usize> {
    needles.iter().filter_map(|n| hay.find(n)).min()
}

/// In-memory mock used by tests and by the `--demo` flag.
///
/// Compiled into the release binary unconditionally so screenshots and gifs
/// can be produced on machines that don't have cmux installed (e.g. CI).
pub mod mock {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[derive(Debug, Default)]
    pub struct MockCmux {
        pub available: bool,
        pub opens: RefCell<Vec<PathBuf>>,
        pub closes: RefCell<Vec<SurfaceId>>,
        /// Next surface id to return from open_markdown.
        pub next_surface_id: RefCell<usize>,
    }

    impl MockCmux {
        pub fn new() -> Self {
            Self {
                available: true,
                opens: RefCell::new(Vec::new()),
                closes: RefCell::new(Vec::new()),
                next_surface_id: RefCell::new(1000),
            }
        }
    }

    impl CmuxClient for MockCmux {
        fn is_available(&self) -> bool {
            self.available
        }
        fn open_markdown(&self, path: &std::path::Path) -> Result<OpenResult, CmuxError> {
            self.opens.borrow_mut().push(path.to_path_buf());
            let mut n = self.next_surface_id.borrow_mut();
            *n += 1;
            Ok(OpenResult {
                surface: SurfaceId(format!("surface:{}", *n)),
                pane: format!("pane:{}", *n),
                path: path.display().to_string(),
            })
        }
        fn close_surface(&self, surface: &SurfaceId) -> Result<(), CmuxError> {
            self.closes.borrow_mut().push(surface.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_real_cmux_output() {
        let s = "OK surface=surface:429 pane=pane:366 path=/tmp/cmux-md-test/test1.md\n";
        let parsed = parse_open_output(s).unwrap();
        assert_eq!(parsed.surface.0, "surface:429");
        assert_eq!(parsed.pane, "pane:366");
        assert_eq!(parsed.path, "/tmp/cmux-md-test/test1.md");
    }

    #[test]
    fn parse_path_with_spaces() {
        let s = "OK surface=surface:1 pane=pane:2 path=/tmp/path with spaces/file.md\n";
        let parsed = parse_open_output(s).unwrap();
        assert_eq!(parsed.surface.0, "surface:1");
        assert_eq!(parsed.path, "/tmp/path with spaces/file.md");
    }

    #[test]
    fn parse_missing_surface_is_error() {
        let s = "OK pane=pane:1 path=/tmp/a.md\n";
        let err = parse_open_output(s).err().unwrap();
        match err {
            CmuxError::ParseError(_) => {}
            _ => panic!("expected ParseError"),
        }
    }

    #[test]
    fn parse_no_ok_line_is_error() {
        let s = "some garbage\n";
        let err = parse_open_output(s).err().unwrap();
        match err {
            CmuxError::ParseError(_) => {}
            _ => panic!("expected ParseError"),
        }
    }

    #[test]
    fn surface_id_as_arg() {
        let id = SurfaceId("surface:42".to_string());
        assert_eq!(id.as_arg(), "surface:42");
    }

    /// A path that happens to contain the substring `surface=` used to steal
    /// the surface id from the real one in the same line. After the
    /// positional rewrite, the first `surface=` wins and a later literal in
    /// the path is treated as plain text.
    #[test]
    fn parse_does_not_let_path_hijack_surface() {
        let s = "OK surface=surface:7 pane=pane:9 path=/tmp/weird surface=evil/file.md\n";
        let parsed = parse_open_output(s).unwrap();
        assert_eq!(parsed.surface.0, "surface:7");
        assert_eq!(parsed.pane, "pane:9");
        assert_eq!(parsed.path, "/tmp/weird surface=evil/file.md");
    }

    #[test]
    fn parse_missing_pane_is_ok() {
        let s = "OK surface=surface:1 path=/tmp/a.md\n";
        let parsed = parse_open_output(s).unwrap();
        assert_eq!(parsed.surface.0, "surface:1");
        assert_eq!(parsed.pane, "");
        assert_eq!(parsed.path, "/tmp/a.md");
    }
}
