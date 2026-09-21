//! Runs the real `bbsadmin` binary against a throwaway database.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use rust_bbs::auth;
use rust_bbs::db::{Db, Role};

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("bbsadmin-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Creating the database is the server's job; here we just open it once.
        Db::open(&dir.join("bbs.db")).unwrap();
        Self { dir }
    }

    fn db(&self) -> Db {
        Db::open(&self.dir.join("bbs.db")).unwrap()
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bbsadmin"))
            .args(args)
            .env("BBS_DATA_DIR", &self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(text) = stdin {
            child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args, None);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args, None);
        assert!(!out.status.success(), "{args:?} should have failed");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn creates_users_with_working_password_hashes() {
    let f = Fixture::new("users");
    let out = f.run(&["user", "add", "sysop", "--sysop", "--password-stdin"], Some("long-enough-pw\n"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let db = f.db();
    let user = db.find_user("SYSOP").unwrap().unwrap();
    assert_eq!(user.role, Role::Sysop);
    assert!(auth::verify_password("long-enough-pw", &user.password_hash));
    assert!(!auth::verify_password("wrong", &user.password_hash));

    // A reserved name is fine for an admin, an invalid one or a weak password is not.
    let bad = f.run(&["user", "add", "1bad", "--password-stdin"], Some("long-enough-pw\n"));
    assert!(!bad.status.success());
    let weak = f.run(&["user", "add", "weakling", "--password-stdin"], Some("short\n"));
    assert!(!weak.status.success());
    assert!(db.account_by_name("weakling").unwrap().is_none());
}

#[test]
fn roles_bans_and_passwords() {
    let f = Fixture::new("roles");
    f.run(&["user", "add", "alice", "--password-stdin"], Some("first-password\n"));
    let db = f.db();
    let id = db.account_by_name("alice").unwrap().unwrap().id;

    f.ok(&["user", "promote", "alice"]);
    assert_eq!(db.account(id).unwrap().unwrap().role, Role::Sysop);
    f.ok(&["user", "demote", "alice"]);
    assert_eq!(db.account(id).unwrap().unwrap().role, Role::User);

    f.ok(&["user", "ban", "alice", "--reason", "testing"]);
    assert!(db.account(id).unwrap().unwrap().banned);
    assert!(f.ok(&["user", "show", "alice"]).contains("banned (testing)"));
    f.ok(&["user", "unban", "alice"]);
    assert!(!db.account(id).unwrap().unwrap().banned);

    f.run(&["user", "passwd", "alice", "--password-stdin"], Some("second-password\n"));
    let hash = db.find_user("alice").unwrap().unwrap().password_hash;
    assert!(auth::verify_password("second-password", &hash));
    assert!(!auth::verify_password("first-password", &hash));

    assert!(f.fails(&["user", "ban", "nobody"]).contains("no such user"));
}

#[test]
fn boards_and_content() {
    let f = Fixture::new("content");
    f.run(&["user", "add", "alice", "--password-stdin"], Some("first-password\n"));
    let db = f.db();
    let uid = db.account_by_name("alice").unwrap().unwrap().id;

    f.ok(&["board", "add", "Retro", "--description", "Old machines"]);
    let board = db.list_boards(None).unwrap().into_iter().find(|b| b.name == "Retro").unwrap();
    assert!(f.fails(&["board", "add", "retro"]).contains("already exists"));
    f.ok(&["board", "rename", &board.id.to_string(), "Retro Computing"]);
    assert_eq!(db.board_name(board.id).unwrap().unwrap(), "Retro Computing");

    let thread = db.create_thread(board.id, uid, "Amiga", "first").unwrap();
    db.add_post(thread, uid, "second").unwrap();
    let posts = db.list_posts(thread, None).unwrap();

    // A board with threads is only deleted with --force.
    assert!(f.fails(&["board", "delete", &board.id.to_string()]).contains("--force"));
    f.ok(&["post", "delete", &posts[1].id.to_string()]);
    assert_eq!(db.list_posts(thread, None).unwrap().len(), 1);
    let out = f.ok(&["post", "delete", &posts[0].id.to_string()]);
    assert!(out.contains("thread") && out.contains("gone"));
    assert!(db.thread_head(thread).unwrap().is_none());

    let t2 = db.create_thread(board.id, uid, "Atari", "text").unwrap();
    f.ok(&["thread", "delete", &t2.to_string()]);
    f.ok(&["board", "delete", &board.id.to_string()]);
    assert!(db.board_name(board.id).unwrap().is_none());
}

#[test]
fn refuses_to_create_a_database_by_accident() {
    let f = Fixture::new("missing");
    let out = Command::new(env!("CARGO_BIN_EXE_bbsadmin"))
        .args(["stats"])
        .env("BBS_DATA_DIR", f.dir.join("nope"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no database"));
    assert!(!f.dir.join("nope").exists());
}
