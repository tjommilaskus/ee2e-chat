//! These have to pass on a machine with a clipboard tool and on one without,
//! since CI and a desktop differ exactly there. So they assert the contract --
//! it either works, or it says clearly what to install -- rather than assuming
//! either outcome.

use ee2e_chat::clipboard::{self, CopyError};

fn any_tool_installed() -> bool {
    ["wl-copy", "xclip", "xsel"]
        .iter()
        .any(|tool| which(tool).is_some())
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
            ["wl-copy", "xclip", "xsel"].contains(&tool),
            "reported an unexpected tool: {tool}"
        ),
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
#[test]
fn test_the_missing_tool_message_says_what_to_install() {
    let message = CopyError::NoTool.to_string();

    assert!(message.contains("wl-clipboard"), "got: {message}");
    assert!(message.contains("xclip"), "got: {message}");
    assert!(message.contains("Wayland"), "got: {message}");
}

/// Copying is a convenience. Failing it must never be able to stop the program,
/// which is why it returns an error rather than panicking.
#[test]
fn test_copying_never_panics() {
    let _ = clipboard::copy("");
    let _ = clipboard::copy(&"x".repeat(100_000));
    let _ = clipboard::copy("a line\nand another");
}
