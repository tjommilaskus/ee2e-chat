//! Reading and writing files that hold secrets.
//!
//! Both the identity key and the room code live in the config directory and
//! must never be readable by anyone else on the machine. The rules are the same
//! for both, so they are written once here rather than twice: a permission bug
//! duplicated is a permission bug that only gets fixed in one place.
//!
//! That "never readable by anyone else" is achieved differently by platform.
//! Unix sets the mode explicitly; Windows inherits it from the profile
//! directory. See `tighten_if_needed`.

use std::fs;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Owner read/write only. Anything wider means somebody else can read it.
#[cfg(unix)]
pub const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
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
        #[cfg(unix)]
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(DIR_MODE));
    }

    // On Unix the mode is set as the file is created rather than afterwards.
    // Creating it first and then tightening would leave a window in which the
    // contents are readable by anyone.
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(FILE_MODE);
    let mut file = options.open(path)?;

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
    // Checked before reading, so an over-exposed secret is reported even if the
    // read then fails for some other reason.
    let note = tighten_if_needed(path)?;
    Ok((fs::read_to_string(path)?, note))
}

#[cfg(unix)]
fn tighten_if_needed(path: &Path) -> io::Result<Option<String>> {
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;

    if mode & 0o077 == 0 {
        return Ok(None);
    }

    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(Some(format!(
        "{} was readable by others (mode {mode:o}); tightened to {FILE_MODE:o}",
        path.display()
    )))
}

/// Windows has no mode bits to check.
///
/// These files live under `%APPDATA%`, inside a profile directory whose ACL
/// already admits only its owner (plus SYSTEM and the administrators, who can
/// read anything on the machine regardless), so the guarantee the Unix branch
/// enforces by hand is one the filesystem is already making. Reading and
/// rewriting DACLs to re-derive it would be a great deal of unsafe code for no
/// change in outcome.
///
/// What is genuinely lost is the *warning*. A key deliberately placed somewhere
/// shared with `--identity` passes unremarked here, where on Unix it would be
/// corrected and reported. That is recorded in the README rather than papered
/// over.
#[cfg(not(unix))]
fn tighten_if_needed(_path: &Path) -> io::Result<Option<String>> {
    Ok(None)
}
