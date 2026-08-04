//! `chat update` -- fetch and install the latest version.
//!
//! Delegates to the same install script the README hands out, so there is one
//! description of how installing works rather than two that can disagree. The
//! script is downloaded to a file and run with arguments rather than piped
//! through a shell, so a path containing spaces or quotes cannot turn into
//! something else on the way.
//!
//! Replacing the running binary is safe: the script installs under a temporary
//! name and renames it into place, which swaps the directory entry rather than
//! writing through the inode this process is executing from.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

const INSTALL_URL: &str =
    "https://raw.githubusercontent.com/tjommilaskus/ee2e-chat/main/install.sh";

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let prefix = prefix_for(&exe)?;

    println!("installed at {}", exe.display());
    println!("current version {}", env!("CARGO_PKG_VERSION"));
    println!();
    // Said out loud, because this runs a script fetched over the network and
    // that should never be a surprise.
    println!("fetching {INSTALL_URL}");

    // Noted so the install can be checked to have actually replaced *this*
    // binary. An installer that writes under a different name would otherwise
    // report success having changed nothing here, and the version printed
    // afterwards would be this same unchanged copy answering.
    let before = modified(&exe);

    let script = download()?;
    let result = install(&script, &prefix);
    let _ = std::fs::remove_file(&script);
    result?;

    println!();
    if modified(&exe) == before {
        return Err(format!(
            "the installer ran, but {} was not replaced.\n\nIt may have \
             installed under a different name; check {} and remove anything \
             stale.",
            exe.display(),
            prefix.join("bin").display()
        )
        .into());
    }

    match Command::new(&exe).arg("--version").output() {
        Ok(out) if out.status.success() => {
            print!("now running {}", String::from_utf8_lossy(&out.stdout));
        }
        // The binary demonstrably changed, so failing to read a version back is
        // worth saying but is not itself a failed update.
        _ => println!("updated, but could not read the new version back"),
    }

    Ok(())
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Work out what `--prefix` this copy was installed with.
///
/// Refused unless the binary sits in a `bin` directory, because anywhere else
/// means this is a development build or an unusual layout, and guessing a
/// prefix there could install over something unrelated.
fn prefix_for(exe: &Path) -> Result<PathBuf, String> {
    let bin_dir = exe
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", exe.display()))?;

    if bin_dir.file_name() != Some(OsStr::new("bin")) {
        return Err(format!(
            "this copy is not in a bin directory ({}), so it was probably built \
             rather than installed.\n\nUpdate it with `git pull && cargo build --release`, \
             or install it properly with install.sh.",
            bin_dir.display()
        ));
    }

    bin_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("could not work out a prefix from {}", bin_dir.display()))
}

fn download() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!("chat-install-{}.sh", std::process::id()));

    let status = Command::new("curl")
        .arg("-fsSL")
        .arg(INSTALL_URL)
        .arg("-o")
        .arg(&path)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "curl is needed to update, and is not installed".to_string()
            }
            _ => format!("could not run curl: {e}"),
        })?;

    if !status.success() {
        return Err(format!("could not download {INSTALL_URL}").into());
    }

    Ok(path)
}

fn install(script: &Path, prefix: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("sh")
        .arg(script)
        .arg("--prefix")
        .arg(prefix)
        .status()
        .map_err(|e| format!("could not run the installer: {e}"))?;

    if !status.success() {
        return Err("the update failed; your existing copy is untouched".into());
    }

    Ok(())
}
