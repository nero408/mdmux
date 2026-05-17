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

use std::process::Command;

use thiserror::Error;

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
        Command::new(self.bin())
            .arg("ping")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn open_markdown(&self, path: &std::path::Path) -> Result<OpenResult, CmuxError> {
        let out = Command::new(self.bin())
            .arg("markdown")
            .arg("open")
            .arg(path)
            .output()?;
        if !out.status.success() {
            return Err(CmuxError::CommandFailed(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        parse_open_output(&stdout)
    }

    fn close_surface(&self, surface: &SurfaceId) -> Result<(), CmuxError> {
        let out = Command::new(self.bin())
            .arg("close-surface")
            .arg("--surface")
            .arg(surface.as_arg())
            .output()?;
        if !out.status.success() {
            return Err(CmuxError::CommandFailed(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        Ok(())
    }
}

/// Parse a line of the form:
///   `OK surface=surface:429 pane=pane:366 path=/some/file.md`
/// Whitespace-separated key=value pairs after the leading "OK".
pub fn parse_open_output(s: &str) -> Result<OpenResult, CmuxError> {
    let line = s
        .lines()
        .find(|l| l.trim_start().starts_with("OK"))
        .ok_or_else(|| CmuxError::ParseError(format!("no OK line in cmux output: {}", s.trim())))?;
    let mut surface: Option<String> = None;
    let mut pane: Option<String> = None;
    let mut path: Option<String> = None;
    for tok in line.split_whitespace().skip(1) {
        if let Some(v) = tok.strip_prefix("surface=") {
            surface = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("pane=") {
            pane = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("path=") {
            path = Some(v.to_string());
        }
    }
    // `path=` may legitimately contain spaces; recover by re-splitting once we
    // know the prefix is found.
    if path.is_some()
        && let Some(idx) = line.find("path=")
    {
        path = Some(line[idx + "path=".len()..].to_string());
    }
    let surface = surface
        .ok_or_else(|| CmuxError::ParseError(format!("missing surface=... in line: {}", line)))?;
    let pane = pane.unwrap_or_default();
    let path = path.unwrap_or_default();
    Ok(OpenResult {
        surface: SurfaceId(surface),
        pane,
        path,
    })
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
}
