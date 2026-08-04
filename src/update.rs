//! `chat update` -- fetch and install the latest version.
//!
//! Clones the repository and runs the install script out of that clone, rather
//! than downloading the script on its own. Two reasons: the script installs
//! from a checkout when it finds one beside itself, so this fetches the source
//! once instead of twice; and GitHub's raw host caches for several minutes,
//! which is long enough to hand back the previous script immediately after a
//! release -- exactly when being current matters most. A clone is never stale.
//!
//! Delegating to that script rather than reimplementing its steps keeps one
//! description of how installing works instead of two that could drift.
//!
//! Replacing the running binary is safe: the script installs under a temporary
//! name and renames it into place, which swaps the directory entry rather than
//! writing through the inode this process is executing from.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO_URL: &str = "https://github.com/tjommilaskus/ee2e-chat.git";

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let prefix = prefix_for(&exe)?;

    println!("installed at {}", exe.display());
    println!("current version {}", env!("CARGO_PKG_VERSION"));
    println!();
    // Said out loud, because this fetches and runs code over the network and
    // that should never be a surprise.
    println!("fetching {REPO_URL}");

    // Noted so the install can be checked to have actually replaced *this*
    // binary. An installer that writes under a different name would otherwise
    // report success having changed nothing here, and the version printed
    // afterwards would be this same unchanged copy answering.
    let before = modified(&exe);

    let workspace = TempDir::new()?;
    let source = workspace.path().join("src");

    clone(&source)?;
    install(&source, &prefix)?;

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

fn clone(into: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(REPO_URL)
        .arg(into)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "git is needed to update, and is not installed".to_string()
            }
            _ => format!("could not run git: {e}"),
        })?;

    if !status.success() {
        return Err(format!("could not clone {REPO_URL}").into());
    }

    Ok(())
}

fn install(source: &Path, prefix: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let script = source.join("install.sh");
    if !script.is_file() {
        return Err("the repository has no install.sh; update it by hand".into());
    }

    // Arguments rather than a shell string, so a prefix containing spaces or
    // quotes cannot turn into something else on the way.
    let status = Command::new("sh")
        .arg(&script)
        .arg("--prefix")
        .arg(prefix)
        .status()
        .map_err(|e| format!("could not run the installer: {e}"))?;

    if !status.success() {
        return Err("the update failed; your existing copy is untouched".into());
    }

    Ok(())
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// A directory removed when it goes out of scope, including on the early
/// returns above -- so a failed update does not leave a clone behind.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("chat-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(TempDir(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
