use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use russh::keys::PrivateKey;
use russh::keys::ssh_key::{Algorithm, LineEnding};
use russh::server::{Config, Server as _};
use russh::{MethodKind, MethodSet};
use tokio::sync::Semaphore;

use rust_bbs::db::Db;
use rust_bbs::server::BbsServer;
use rust_bbs::state::{Limiter, Shared};
use rust_bbs::auth;

/// Concurrent Argon2 hashes allowed (each needs ~19 MiB of memory).
const MAX_CONCURRENT_HASHES: usize = 4;

/// Loads the host key from `path`, or creates and saves one on first start, so
/// clients keep trusting the server across restarts.
fn load_or_create_host_key(path: &Path) -> anyhow::Result<PrivateKey> {
    if path.exists() {
        return PrivateKey::read_openssh_file(path)
            .with_context(|| format!("reading host key {}", path.display()));
    }
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    key.write_openssh_file(path, LineEnding::LF)
        .with_context(|| format!("writing host key {}", path.display()))?;
    println!("generated new host key at {}", path.display());
    Ok(key)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data_dir = PathBuf::from(std::env::var("BBS_DATA_DIR").unwrap_or_else(|_| "data".into()));
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;

    let host_key = load_or_create_host_key(&data_dir.join("host_key"))?;
    let db = Db::open(&data_dir.join("bbs.db")).context("opening database")?;
    let guest_password = std::env::var("BBS_GUEST_PASSWORD").unwrap_or_else(|_| "letmein".into());
    let dummy_hash = auth::hash_password("not-a-real-password").map_err(anyhow::Error::msg)?;

    let shared = Arc::new(Shared {
        db,
        guest_password,
        hash_slots: Semaphore::new(MAX_CONCURRENT_HASHES),
        limiter: Arc::new(Limiter::default()),
        dummy_hash,
    });

    let config = Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(3600)),
        auth_rejection_time: std::time::Duration::from_secs(1),
        auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
        // Only what we implement; no "none" or keyboard-interactive.
        methods: MethodSet::from(&[MethodKind::Password, MethodKind::PublicKey][..]),
        max_auth_attempts: 6,
        keys: vec![host_key],
        nodelay: true,
        ..Default::default()
    };

    let port = std::env::var("BBS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(2222u16);
    let addr = ("0.0.0.0", port);
    println!("rust-bbs listening on {}:{}", addr.0, addr.1);
    println!("data directory: {}", data_dir.display());
    println!("guest login: username 'guest' (password from BBS_GUEST_PASSWORD, default 'letmein')");
    println!("connect with: ssh -p {port} guest@localhost");

    let mut server = BbsServer::new(shared);
    server.run_on_address(Arc::new(config), addr).await?;
    Ok(())
}
