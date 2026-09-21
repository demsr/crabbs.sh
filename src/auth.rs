use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use russh::keys::{HashAlg, PublicKey};

pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 16;
pub const PASSWORD_MIN: usize = 8;
/// Upper bound so nobody can make us hash megabytes of input.
pub const PASSWORD_MAX: usize = 128;

const RESERVED_NAMES: &[&str] = &[
    "guest",
    "admin",
    "administrator",
    "root",
    "sysop",
    "system",
    "bbs",
    "anonymous",
    "anon",
    "null",
    "nobody",
    "everyone",
    "moderator",
    "mod",
    "support",
    "staff",
];

/// Syntax rules only. The admin tool uses this directly so a sysop can create
/// accounts with otherwise reserved names such as "sysop".
pub fn validate_username_format(name: &str) -> Result<(), String> {
    let len = name.chars().count();
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&len) {
        return Err(format!(
            "Username must be {USERNAME_MIN}-{USERNAME_MAX} characters long."
        ));
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err("Username must start with a letter.".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("Username may only contain letters, digits, '_' and '-'.".into());
    }
    Ok(())
}

/// Rules for self-registration: the syntax rules plus reserved names.
pub fn validate_username(name: &str) -> Result<(), String> {
    validate_username_format(name)?;
    if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        return Err("That username is reserved.".into());
    }
    Ok(())
}

pub fn validate_password(username: &str, password: &str) -> Result<(), String> {
    let len = password.chars().count();
    if len < PASSWORD_MIN {
        return Err(format!(
            "Password must be at least {PASSWORD_MIN} characters."
        ));
    }
    if len > PASSWORD_MAX {
        return Err(format!(
            "Password must be at most {PASSWORD_MAX} characters."
        ));
    }
    if password.chars().any(char::is_control) {
        return Err("Password must not contain control characters.".into());
    }
    if password.eq_ignore_ascii_case(username) {
        return Err("Password must not be the same as the username.".into());
    }
    Ok(())
}

/// Hashes with Argon2id (the crate's default parameters, which follow the
/// OWASP minimum) and a fresh random salt. CPU- and memory-heavy: call from
/// `spawn_blocking` and bound concurrency (see `Shared::hash_slots`).
pub fn hash_password(password: &str) -> Result<String, String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|err| format!("password hashing failed: {err}"))
}

pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Parses a pasted `authorized_keys`-style line. Returns the key and a
/// display-safe comment.
pub fn parse_public_key(line: &str) -> Result<(PublicKey, String), String> {
    let key = PublicKey::from_openssh(line.trim()).map_err(|_| {
        "That doesn't look like an OpenSSH public key (e.g. 'ssh-ed25519 AAAA… comment')."
            .to_string()
    })?;
    if matches!(key.algorithm(), russh::keys::Algorithm::Dsa) {
        return Err("DSA keys are not supported.".into());
    }
    let comment: String = key
        .comment()
        .to_string()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    Ok((key, comment))
}
