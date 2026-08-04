//! Putting text on the system clipboard.
//!
//! Done by handing it to whichever clipboard tool is installed rather than by
//! linking a library. On Wayland the clipboard is owned by a live process, not
//! stored centrally, so something has to stay running to serve it; `wl-copy`
//! forks for exactly that purpose. A library doing it in-process would lose the
//! contents the moment the program exits, which for a chat client is precisely
//! when someone wants to paste.

use std::io::Write;
use std::process::{Command, Stdio};

/// Tools to try, in order. Wayland first, since a Wayland session may also have
/// the X11 tools present through XWayland and they would write to the wrong
/// clipboard.
const TOOLS: &[(&str, &[&str])] = &[
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

#[derive(Debug, PartialEq, Eq)]
pub enum CopyError {
    /// None of the known tools are installed.
    NoTool,
    Failed(String),
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopyError::NoTool => write!(
                f,
                "no clipboard tool found -- install wl-clipboard (Wayland) or xclip (X11)"
            ),
            CopyError::Failed(why) => write!(f, "could not copy: {why}"),
        }
    }
}

/// Copy `text`, returning the name of the tool that took it.
pub fn copy(text: &str) -> Result<&'static str, CopyError> {
    let mut last_error = None;

    for (tool, args) in TOOLS {
        match try_tool(tool, args, text) {
            Ok(()) => return Ok(tool),
            // Not installed, so move on rather than reporting it as a failure.
            Err(CopyError::NoTool) => continue,
            Err(e) => last_error = Some(e),
        }
    }

    Err(last_error.unwrap_or(CopyError::NoTool))
}

fn try_tool(tool: &str, args: &[&str], text: &str) -> Result<(), CopyError> {
    let mut child = match Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(CopyError::NoTool),
        Err(e) => return Err(CopyError::Failed(e.to_string())),
    };

    // Dropped after writing, so the tool sees end of input and can get on with
    // it. `wl-copy` in particular waits for the stream to close.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CopyError::Failed("could not write to it".to_string()))?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| CopyError::Failed(e.to_string()))?;
    }

    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(CopyError::Failed(format!("{tool} exited with {status}"))),
        Err(e) => Err(CopyError::Failed(e.to_string())),
    }
}
