//! Storing the long-term keypair between runs.
//!
//! Without this a fresh keypair is generated every start, so a fingerprint
//! proves only that the current connection is not intercepted -- it says
//! nothing about whether this is the same person you spoke to yesterday.
//! Persisting the key is what lets a fingerprint mean anything over time.
//!
//! The file holds a private key, so it is created readable only by its owner
//! and refused if that is not the case.

use crate::crypto::{self, Keypair, KEY_LEN};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Owner read/write only. Anything wider means somebody else can read the key.
const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;

#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    /// Present so the format can change later without silently misreading an
    /// old file as a new one.
    version: u8,
    secret_key: String,
}

const VERSION: u8 = 1;

#[derive(Debug)]
pub enum IdentityError {
    NoConfigDir,
    Io { path: PathBuf, source: std::io::Error },
    Malformed { path: PathBuf, reason: String },
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::NoConfigDir => {
                write!(f, "could not work out where to store the identity: neither $XDG_CONFIG_HOME nor $HOME is set")
            }
            IdentityError::Io { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            IdentityError::Malformed { path, reason } => {
                write!(f, "{} is not a usable identity file: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for IdentityError {}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> IdentityError + '_ {
    move |source| IdentityError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Where the identity lives unless told otherwise, following the XDG spec.
pub fn default_path() -> Result<PathBuf, IdentityError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or(IdentityError::NoConfigDir)?;

    Ok(base.join("ee2e-chat").join("identity"))
}

/// `Debug` is safe to derive here: `Keypair` redacts its own secret key, so
/// printing this cannot leak it.
#[derive(Debug)]
pub struct Loaded {
    pub keypair: Keypair,
    /// No identity existed and one was written.
    pub created: bool,
    /// Things the user should be told, such as permissions having been
    /// tightened.
    pub notes: Vec<String>,
}

/// Read the stored identity, creating one if there is none.
pub fn load_or_create(path: &Path) -> Result<Loaded, IdentityError> {
    if path.exists() {
        let mut notes = Vec::new();
        let keypair = load(path, &mut notes)?;
        return Ok(Loaded {
            keypair,
            created: false,
            notes,
        });
    }

    let keypair = crypto::generate_keypair();
    save(path, &keypair)?;

    Ok(Loaded {
        keypair,
        created: true,
        notes: Vec::new(),
    })
}

fn load(path: &Path, notes: &mut Vec<String>) -> Result<Keypair, IdentityError> {
    // Checked before reading, so an over-exposed key is reported even if the
    // read then fails for some other reason.
    tighten_if_needed(path, notes)?;

    let text = fs::read_to_string(path).map_err(io_err(path))?;

    let stored: StoredIdentity =
        serde_json::from_str(&text).map_err(|e| IdentityError::Malformed {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

    if stored.version != VERSION {
        return Err(IdentityError::Malformed {
            path: path.to_path_buf(),
            reason: format!(
                "it is version {}, and this build understands version {VERSION}",
                stored.version
            ),
        });
    }

    let secret_key = from_hex(&stored.secret_key).ok_or_else(|| IdentityError::Malformed {
        path: path.to_path_buf(),
        reason: "the secret key is not valid hex".to_string(),
    })?;

    if secret_key.len() != KEY_LEN {
        return Err(IdentityError::Malformed {
            path: path.to_path_buf(),
            reason: format!("the secret key is {} bytes, expected {KEY_LEN}", secret_key.len()),
        });
    }

    // Derived rather than stored, so the file cannot hold a public key that
    // disagrees with its secret key.
    let public_key = crypto::public_key_for(&secret_key).map_err(|e| IdentityError::Malformed {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;

    Ok(Keypair {
        public_key,
        secret_key,
    })
}

/// Complain about, and correct, a key file others can read.
fn tighten_if_needed(path: &Path, notes: &mut Vec<String>) -> Result<(), IdentityError> {
    let mode = fs::metadata(path).map_err(io_err(path))?.permissions().mode() & 0o777;

    if mode & 0o077 == 0 {
        return Ok(());
    }

    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE)).map_err(io_err(path))?;
    notes.push(format!(
        "{} was readable by others (mode {mode:o}); tightened to {FILE_MODE:o}",
        path.display()
    ));
    Ok(())
}

fn save(path: &Path, keypair: &Keypair) -> Result<(), IdentityError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_err(parent))?;
        // Best effort: an existing config directory may legitimately be shared,
        // and failing here would be worse than leaving it as the user set it.
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(DIR_MODE));
    }

    let stored = StoredIdentity {
        version: VERSION,
        secret_key: to_hex(&keypair.secret_key),
    };
    let text = serde_json::to_string_pretty(&stored).map_err(|e| IdentityError::Malformed {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;

    // The mode is set as the file is created rather than afterwards. Creating
    // it first and then tightening would leave a window in which the private
    // key is readable by anyone.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)
        .map_err(io_err(path))?;

    file.write_all(text.as_bytes()).map_err(io_err(path))?;
    file.write_all(b"\n").map_err(io_err(path))?;

    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}
