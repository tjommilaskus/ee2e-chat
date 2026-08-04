//! Reading and writing files that hold secrets.
//!
//! Both the identity key and the room code live in the config directory and
//! must never be readable by anyone else on the machine. The rules are the same
//! for both, so they are written once here rather than twice: a permission bug
//! duplicated is a permission bug that only gets fixed in one place.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Owner read/write only. Anything wider means somebody else can read it.
pub const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;

/// Create a file only this user can read, and write `contents` to it.
///
/// Fails if the file already exists, so an accidental overwrite of a key is not
/// possible through this path.
pub fn write_new(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        // Best effort: an existing config directory may legitimately be shared
        // with other tools, and failing here would be worse than leaving it as
        // the user set it.
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(DIR_MODE));
    }

    // The mode is set as the file is created rather than afterwards. Creating
    // it first and then tightening would leave a window in which the contents
    // are readable by anyone.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)?;

    file.write_all(contents.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Read a secret file, tightening its permissions first if they are too open.
///
/// Returns the contents and, when it had to correct them, a note for the user.
/// Correcting quietly would hide the fact that the secret may already have been
/// read by somebody else.
pub fn read_existing(path: &Path) -> io::Result<(String, Option<String>)> {
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;

    let mut note = None;
    if mode & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
        note = Some(format!(
            "{} was readable by others (mode {mode:o}); tightened to {FILE_MODE:o}",
            path.display()
        ));
    }

    Ok((fs::read_to_string(path)?, note))
}
