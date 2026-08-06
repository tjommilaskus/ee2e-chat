use ee2e_chat::crypto;
use ee2e_chat::identity::{self, IdentityError};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::TempDir;

fn scratch() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("nested").join("identity");
    (dir, path)
}

#[cfg(unix)]
fn mode_of(path: &PathBuf) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The whole point of persisting: the same key comes back next run, so the
/// fingerprint somebody verified yesterday still matches today.
#[test]
fn test_the_same_identity_comes_back_on_the_next_run() {
    let (_dir, path) = scratch();

    let first = identity::load_or_create(&path).expect("create");
    assert!(first.created);

    let second = identity::load_or_create(&path).expect("load");
    assert!(!second.created, "the second run must not create a new one");

    assert_eq!(first.keypair.secret_key, second.keypair.secret_key);
    assert_eq!(first.keypair.public_key, second.keypair.public_key);
    assert_eq!(
        crypto::fingerprint(&first.keypair.public_key),
        crypto::fingerprint(&second.keypair.public_key),
        "the fingerprint must survive a restart"
    );
}

#[test]
fn test_two_identities_are_different() {
    let (_a, path_a) = scratch();
    let (_b, path_b) = scratch();

    let a = identity::load_or_create(&path_a).expect("create");
    let b = identity::load_or_create(&path_b).expect("create");

    assert_ne!(a.keypair.secret_key, b.keypair.secret_key);
}

// ---------------------------------------------------------------------------
// File permissions
//
// Unix only. Windows has no mode bits: these files sit under %APPDATA%, in a
// profile directory whose ACL already restricts them to their owner, so there
// is nothing for this program to set and nothing to assert about. See
// `tighten_if_needed` in src/secretfile.rs.
// ---------------------------------------------------------------------------

/// A private key readable by anyone else on the machine is not private.
#[cfg(unix)]
#[test]
fn test_the_key_file_is_created_private() {
    let (_dir, path) = scratch();
    identity::load_or_create(&path).expect("create");

    assert_eq!(
        mode_of(&path),
        0o600,
        "the identity file must be readable only by its owner"
    );
}

#[cfg(unix)]
#[test]
fn test_the_directory_is_created_private() {
    let (_dir, path) = scratch();
    identity::load_or_create(&path).expect("create");

    let parent = path.parent().unwrap().to_path_buf();
    assert_eq!(mode_of(&parent), 0o700);
}

/// An exposed key is corrected and reported rather than used quietly.
#[cfg(unix)]
#[test]
fn test_an_over_permissive_file_is_tightened_and_reported() {
    let (_dir, path) = scratch();
    identity::load_or_create(&path).expect("create");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    let loaded = identity::load_or_create(&path).expect("load");

    assert_eq!(mode_of(&path), 0o600, "permissions should have been fixed");
    assert!(
        loaded.notes.iter().any(|note| note.contains("readable by others")),
        "the user should be told, got {:?}",
        loaded.notes
    );
}

#[test]
fn test_a_correctly_permissioned_file_is_not_remarked_on() {
    let (_dir, path) = scratch();
    identity::load_or_create(&path).expect("create");

    let loaded = identity::load_or_create(&path).expect("load");
    assert!(loaded.notes.is_empty(), "got {:?}", loaded.notes);
}

// ---------------------------------------------------------------------------
// Damaged files
//
// Every case must fail loudly. Quietly generating a replacement would change
// the fingerprint, which is precisely the signal that means "somebody is
// impersonating them" -- so a corrupt file would be indistinguishable from an
// attack.
// ---------------------------------------------------------------------------

fn write_identity(path: &PathBuf, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    // Written private so these cases test the damage they mean to, rather than
    // tripping the permission warning on the way in.
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn test_a_corrupt_file_is_an_error() {
    let (_dir, path) = scratch();
    write_identity(&path, "this is not json");

    assert!(matches!(
        identity::load_or_create(&path),
        Err(IdentityError::Malformed { .. })
    ));
}

#[test]
fn test_a_future_version_is_an_error() {
    let (_dir, path) = scratch();
    write_identity(&path, r#"{"version":99,"secret_key":"00"}"#);

    match identity::load_or_create(&path) {
        Err(IdentityError::Malformed { reason, .. }) => {
            assert!(reason.contains("version"), "got: {reason}")
        }
        other => panic!("expected a version error, got {other:?}"),
    }
}

#[test]
fn test_a_key_of_the_wrong_length_is_an_error() {
    let (_dir, path) = scratch();
    write_identity(&path, r#"{"version":1,"secret_key":"aabbcc"}"#);

    match identity::load_or_create(&path) {
        Err(IdentityError::Malformed { reason, .. }) => {
            assert!(reason.contains("bytes"), "got: {reason}")
        }
        other => panic!("expected a length error, got {other:?}"),
    }
}

#[test]
fn test_a_key_that_is_not_hex_is_an_error() {
    let (_dir, path) = scratch();
    write_identity(&path, r#"{"version":1,"secret_key":"zzzz"}"#);

    match identity::load_or_create(&path) {
        Err(IdentityError::Malformed { reason, .. }) => {
            assert!(reason.contains("hex"), "got: {reason}")
        }
        other => panic!("expected a hex error, got {other:?}"),
    }
}

/// The stored file carries only the secret key, and the public key is derived
/// from it, so the two can never disagree.
#[test]
fn test_the_public_key_is_derived_not_trusted() {
    let (_dir, path) = scratch();
    let created = identity::load_or_create(&path).expect("create");

    let text = fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains("public"),
        "the public key should not be stored: {text}"
    );

    let loaded = identity::load_or_create(&path).expect("load");
    assert_eq!(
        loaded.keypair.public_key,
        crypto::public_key_for(&created.keypair.secret_key).unwrap()
    );
}

/// The stored key has to actually work for messages, not merely round-trip.
#[test]
fn test_a_reloaded_identity_can_still_decrypt() {
    let (_dir, path) = scratch();
    let alice = identity::load_or_create(&path).expect("create").keypair;
    let bob = crypto::generate_keypair();

    let (ciphertext, nonce) =
        crypto::seal(b"still works", &alice.public_key, &bob.secret_key).unwrap();

    // Reload from disk, as a fresh run would.
    let reloaded = identity::load_or_create(&path).expect("load").keypair;
    let plaintext =
        crypto::open(&ciphertext, &nonce, &reloaded.secret_key, &bob.public_key).unwrap();

    assert_eq!(plaintext, b"still works");
}

/// `$XDG_CONFIG_HOME` on Unix, `%APPDATA%` on Windows -- the assertion is the
/// same either way, since only the shape can be checked: the environment
/// belongs to whoever is running the tests.
#[test]
fn test_default_path_is_under_the_config_directory() {
    let path = identity::default_path().expect("a path");
    // Compared component-wise, so it holds under either separator.
    assert!(
        path.ends_with(PathBuf::from("ee2e-chat").join("identity")),
        "got {}",
        path.display()
    );
    assert!(path.is_absolute());
}

/// `Loaded` derives `Debug` and wraps a private key, so a stray `{:?}` in an
/// error path must not put it on screen.
#[test]
fn test_debug_output_does_not_leak_the_secret_key() {
    let (_dir, path) = scratch();
    let loaded = identity::load_or_create(&path).expect("create");

    let secret_hex: String = loaded
        .keypair
        .secret_key
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    assert!(
        !format!("{loaded:?}").contains(&secret_hex),
        "the secret key must not appear in Debug output"
    );
}
