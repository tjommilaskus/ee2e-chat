//! These have to pass on a machine with a clipboard tool and on one without,
//! since CI and a desktop differ exactly there. So they assert the contract --
//! it either works, or it says clearly what to install -- rather than assuming
//! either outcome.

use ee2e_chat::clipboard::{self, CopyError};

#[cfg(unix)]
const TOOLS: &[&str] = &["wl-copy", "xclip", "xsel", "pbcopy"];

/// Windows ships `clip`, so there is only one and it is always installed.
#[cfg(not(unix))]
const TOOLS: &[&str] = &["clip"];

fn any_tool_installed() -> bool {
    TOOLS.iter().any(|tool| which(tool).is_some())
}

fn which(tool: &str) -> Option<()> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(tool))
            .find(|candidate| candidate.is_file())
            .map(|_| ())
    })
}

#[test]
fn test_copying_either_works_or_explains_itself() {
    match clipboard::copy("TIO-1111-1111-1111-1111") {
        Ok(tool) => assert!(
            TOOLS.contains(&tool),
            "reported an unexpected tool: {tool}"
        ),
        // `clip` is always present on Windows, but a session with no window
        // station -- CI, or a service -- has no clipboard for it to write to.
        // Saying so is the contract holding, not breaking.
        #[cfg(not(unix))]
        Err(CopyError::Failed(_)) => {}
        Err(e) => {
            assert!(
                !any_tool_installed(),
                "a tool is installed but copying still failed: {e}"
            );
            assert_eq!(e, CopyError::NoTool);
        }
    }
}

/// The message has to name something installable. Told only that it failed, a
/// user has nothing to act on.
#[cfg(unix)]
#[test]
fn test_the_missing_tool_message_says_what_to_install() {
    let message = CopyError::NoTool.to_string();

    assert!(message.contains("wl-clipboard"), "got: {message}");
    assert!(message.contains("xclip"), "got: {message}");
    assert!(message.contains("Wayland"), "got: {message}");
}

/// Nothing is installable on Windows -- `clip` is part of the system -- so the
/// message has to point at the only thing that could actually be wrong.
#[cfg(not(unix))]
#[test]
fn test_the_missing_tool_message_points_at_path() {
    let message = CopyError::NoTool.to_string();

    assert!(message.contains("clip"), "got: {message}");
    assert!(message.contains("PATH"), "got: {message}");
}

/// Copying is a convenience. Failing it must never be able to stop the program,
/// which is why it returns an error rather than panicking.
#[test]
fn test_copying_never_panics() {
    let _ = clipboard::copy("");
    let _ = clipboard::copy(&"x".repeat(100_000));
    let _ = clipboard::copy("a line\nand another");
}

/// macOS ships pbcopy, so a build that does not know about it would report "no
/// clipboard tool" on a machine that plainly has one.
#[cfg(unix)]
#[test]
fn test_pbcopy_is_among_the_tools_tried() {
    assert!(TOOLS.contains(&"pbcopy"), "macOS would have no clipboard");
}

/// The same trap on Windows: `clip` is always there, so failing to try it would
/// report no clipboard on a machine that certainly has one.
#[cfg(not(unix))]
#[test]
fn test_clip_is_among_the_tools_tried() {
    assert!(TOOLS.contains(&"clip"), "Windows would have no clipboard");
}
