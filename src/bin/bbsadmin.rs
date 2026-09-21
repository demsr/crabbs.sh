//! Command-line administration for a rust-bbs database.
//!
//! Works directly on the SQLite file, so it can run while the server is up:
//! the server reads roles, bans, boards and posts fresh from the database on
//! every action, and disconnects banned users within seconds.

use std::io::BufRead;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};

use rust_bbs::auth;
use rust_bbs::content;
use rust_bbs::db::{Account, Db, DbError, Role};

#[derive(Parser)]
#[command(name = "bbsadmin", version, about = "Administer a rust-bbs database")]
struct Cli {
    /// Directory containing bbs.db (same as the server's BBS_DATA_DIR)
    #[arg(long, env = "BBS_DATA_DIR", default_value = "data", global = true)]
    data_dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show counts of users, boards, threads and posts
    Stats,
    /// Manage message boards
    #[command(subcommand)]
    Board(BoardCommand),
    /// Manage user accounts
    #[command(subcommand)]
    User(UserCommand),
    /// List and delete threads
    #[command(subcommand)]
    Thread(ThreadCommand),
    /// List and delete posts
    #[command(subcommand)]
    Post(PostCommand),
}

#[derive(Subcommand)]
enum BoardCommand {
    /// List boards in display order
    List,
    /// Create a board (appended after the existing ones)
    Add {
        name: String,
        #[arg(short, long, default_value = "")]
        description: String,
    },
    /// Change a board's name
    Rename { id: i64, name: String },
    /// Change a board's description
    Describe { id: i64, description: String },
    /// Set a board's position; lower numbers are listed first
    Move { id: i64, position: i64 },
    /// Delete a board
    Delete {
        id: i64,
        /// Also delete all threads and posts in it
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct PasswordOpts {
    /// Read the password from the first line of stdin instead of prompting
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Subcommand)]
enum UserCommand {
    /// List all users
    List,
    /// Show one user, including their SSH keys
    Show { name: String },
    /// Create an account
    Add {
        name: String,
        /// Make the new account a sysop
        #[arg(long)]
        sysop: bool,
        #[command(flatten)]
        password: PasswordOpts,
    },
    /// Give a user the sysop role
    Promote { name: String },
    /// Take the sysop role away
    Demote { name: String },
    /// Suspend an account: no login, no posting, connected sessions are dropped
    Ban {
        name: String,
        #[arg(short, long, default_value = "no reason given")]
        reason: String,
    },
    /// Lift a suspension
    Unban { name: String },
    /// Set a new password (there is no self-service password reset)
    Passwd {
        name: String,
        #[command(flatten)]
        password: PasswordOpts,
    },
    /// Remove one SSH key by its id (see `user show`)
    RemoveKey { key_id: i64 },
}

#[derive(Subcommand)]
enum ThreadCommand {
    /// List the threads of a board
    List { board_id: i64 },
    /// Delete a thread and all its posts
    Delete { id: i64 },
}

#[derive(Subcommand)]
enum PostCommand {
    /// List the posts of a thread (with ids)
    List { thread_id: i64 },
    /// Delete a post; the thread goes too if it was the last one
    Delete { id: i64 },
}

fn db_err<T>(result: Result<T, DbError>, what: &str) -> anyhow::Result<T> {
    result.map_err(|e| anyhow::anyhow!("{what}: {e}"))
}

fn account(db: &Db, name: &str) -> anyhow::Result<Account> {
    match db_err(db.account_by_name(name), "looking up user")? {
        Some(account) => Ok(account),
        None => bail!("no such user: {name}"),
    }
}

fn read_password(username: &str, opts: &PasswordOpts) -> anyhow::Result<String> {
    let password = if opts.password_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line.trim_end_matches(['\r', '\n']).to_string()
    } else {
        let first = rpassword::prompt_password("New password: ")?;
        let again = rpassword::prompt_password("Repeat password: ")?;
        if first != again {
            bail!("passwords don't match");
        }
        first
    };
    auth::validate_password(username, &password).map_err(anyhow::Error::msg)?;
    Ok(password)
}

fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_string()
    }
}

fn run(db: &Db, command: Command) -> anyhow::Result<()> {
    match command {
        Command::Stats => {
            let s = db_err(db.stats(), "stats")?;
            println!("users:   {} ({} sysops, {} banned)", s.users, s.sysops, s.banned);
            println!("boards:  {}", s.boards);
            println!("threads: {}", s.threads);
            println!("posts:   {}", s.posts);
            println!("keys:    {}", s.keys);
        }
        Command::Board(cmd) => board(db, cmd)?,
        Command::User(cmd) => user(db, cmd)?,
        Command::Thread(ThreadCommand::List { board_id }) => {
            let Some(name) = db_err(db.board_name(board_id), "board")? else {
                bail!("no such board: {board_id}");
            };
            println!("Board {board_id}: {name}");
            println!("{:>5}  {:<40}  {:<16}  {:>5}  {}", "ID", "TITLE", "AUTHOR", "POSTS", "LAST POST");
            for t in db_err(db.list_threads(board_id), "threads")? {
                println!(
                    "{:>5}  {:<40}  {:<16}  {:>5}  {}",
                    t.id,
                    first_line(&t.title, 40),
                    t.author,
                    t.post_count,
                    t.last_post
                );
            }
        }
        Command::Thread(ThreadCommand::Delete { id }) => {
            let Some(head) = db_err(db.thread_head(id), "thread")? else {
                bail!("no such thread: {id}");
            };
            db_err(db.delete_thread(id), "deleting thread")?;
            println!("Deleted thread {id} \"{}\" from board {}.", head.title, head.board_name);
        }
        Command::Post(PostCommand::List { thread_id }) => {
            let Some(head) = db_err(db.thread_head(thread_id), "thread")? else {
                bail!("no such thread: {thread_id}");
            };
            println!("Thread {thread_id}: {} (board {})", head.title, head.board_name);
            println!("{:>5}  {:<16}  {:<16}  {}", "ID", "AUTHOR", "POSTED", "TEXT");
            for p in db_err(db.list_posts(thread_id), "posts")? {
                println!(
                    "{:>5}  {:<16}  {:<16}  {}",
                    p.id,
                    p.author,
                    p.created,
                    first_line(&p.body, 50)
                );
            }
        }
        Command::Post(PostCommand::Delete { id }) => {
            let d = db_err(db.delete_post(id), "deleting post")?;
            if d.thread_deleted {
                println!("Deleted post {id}; it was the last one, so thread {} is gone too.", d.thread_id);
            } else {
                println!("Deleted post {id} from thread {}.", d.thread_id);
            }
        }
    }
    Ok(())
}

fn board(db: &Db, cmd: BoardCommand) -> anyhow::Result<()> {
    match cmd {
        BoardCommand::List => {
            println!("{:>4}  {:>3}  {:<16}  {:>7}  {}", "ID", "POS", "NAME", "THREADS", "DESCRIPTION");
            for b in db_err(db.list_boards(), "boards")? {
                println!(
                    "{:>4}  {:>3}  {:<16}  {:>7}  {}",
                    b.id, b.position, b.name, b.thread_count, b.description
                );
            }
        }
        BoardCommand::Add { name, description } => {
            let name = content::clean_title(&name).map_err(anyhow::Error::msg)?;
            let description = if description.trim().is_empty() {
                String::new()
            } else {
                content::clean_title(&description).map_err(anyhow::Error::msg)?
            };
            let id = match db.create_board(&name, &description) {
                Ok(id) => id,
                Err(DbError::Duplicate) => bail!("a board named \"{name}\" already exists"),
                Err(e) => bail!("creating board: {e}"),
            };
            println!("Created board {id}: {name}");
        }
        BoardCommand::Rename { id, name } => {
            let name = content::clean_title(&name).map_err(anyhow::Error::msg)?;
            db_err(db.update_board(id, Some(&name), None, None), "renaming board")?;
            println!("Board {id} is now called \"{name}\".");
        }
        BoardCommand::Describe { id, description } => {
            let description = content::clean_title(&description).map_err(anyhow::Error::msg)?;
            db_err(db.update_board(id, None, Some(&description), None), "updating board")?;
            println!("Updated the description of board {id}.");
        }
        BoardCommand::Move { id, position } => {
            db_err(db.update_board(id, None, None, Some(position)), "moving board")?;
            println!("Board {id} is now at position {position}.");
        }
        BoardCommand::Delete { id, force } => {
            match db.delete_board(id, force) {
                Ok(()) => println!("Deleted board {id}."),
                Err(DbError::NotEmpty) => {
                    bail!("board {id} still has threads; use --force to delete them and their posts")
                }
                Err(e) => bail!("deleting board: {e}"),
            }
        }
    }
    Ok(())
}

fn user(db: &Db, cmd: UserCommand) -> anyhow::Result<()> {
    match cmd {
        UserCommand::List => {
            println!(
                "{:>4}  {:<16}  {:<6}  {:<7}  {:>5}  {:>4}  {}",
                "ID", "NAME", "ROLE", "STATUS", "POSTS", "KEYS", "CREATED"
            );
            for u in db_err(db.list_users(), "users")? {
                println!(
                    "{:>4}  {:<16}  {:<6}  {:<7}  {:>5}  {:>4}  {}",
                    u.id,
                    u.username,
                    u.role.as_str(),
                    if u.banned { "banned" } else { "ok" },
                    u.posts,
                    u.keys,
                    u.created
                );
            }
        }
        UserCommand::Show { name } => {
            let a = account(db, &name)?;
            let summary = db_err(db.list_users(), "users")?
                .into_iter()
                .find(|u| u.id == a.id)
                .context("user vanished")?;
            println!("id:       {}", a.id);
            println!("name:     {}", a.username);
            println!("role:     {}", a.role.as_str());
            match (&summary.banned, &summary.ban_reason) {
                (true, Some(reason)) => println!("status:   banned ({reason})"),
                (true, None) => println!("status:   banned"),
                _ => println!("status:   ok"),
            }
            println!("created:  {}", summary.created);
            println!("posts:    {}", summary.posts);
            let keys = db_err(db.list_keys(a.id), "keys")?;
            println!("ssh keys: {}", keys.len());
            for k in keys {
                println!("  [{}] {}  {}", k.id, k.fingerprint, k.comment);
            }
        }
        UserCommand::Add {
            name,
            sysop,
            password,
        } => {
            auth::validate_username_format(&name).map_err(anyhow::Error::msg)?;
            let password = read_password(&name, &password)?;
            let hash = auth::hash_password(&password).map_err(anyhow::Error::msg)?;
            let id = match db.create_user(&name, &hash) {
                Ok(id) => id,
                Err(DbError::Duplicate) => bail!("a user named {name} already exists"),
                Err(e) => bail!("creating user: {e}"),
            };
            if sysop {
                db_err(db.set_role(id, Role::Sysop), "setting role")?;
            }
            println!("Created {} {name} (id {id}).", if sysop { "sysop" } else { "user" });
        }
        UserCommand::Promote { name } => set_role(db, &name, Role::Sysop)?,
        UserCommand::Demote { name } => set_role(db, &name, Role::User)?,
        UserCommand::Ban { name, reason } => {
            let a = account(db, &name)?;
            db_err(db.set_banned(a.id, Some(&reason)), "banning")?;
            println!(
                "Banned {}. Connected sessions are dropped within about 10 seconds.",
                a.username
            );
        }
        UserCommand::Unban { name } => {
            let a = account(db, &name)?;
            db_err(db.set_banned(a.id, None), "unbanning")?;
            println!("Unbanned {}.", a.username);
        }
        UserCommand::Passwd { name, password } => {
            let a = account(db, &name)?;
            let password = read_password(&a.username, &password)?;
            let hash = auth::hash_password(&password).map_err(anyhow::Error::msg)?;
            db_err(db.set_password_hash(a.id, &hash), "setting password")?;
            println!("Password for {} changed.", a.username);
        }
        UserCommand::RemoveKey { key_id } => {
            db_err(db.delete_key_by_id(key_id), "removing key")?;
            println!("Removed key {key_id}.");
        }
    }
    Ok(())
}

fn set_role(db: &Db, name: &str, role: Role) -> anyhow::Result<()> {
    let a = account(db, name)?;
    if a.role == role {
        println!("{} is already {}.", a.username, role.as_str());
        return Ok(());
    }
    db_err(db.set_role(a.id, role), "setting role")?;
    println!("{} is now {}.", a.username, role.as_str());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let path = cli.data_dir.join("bbs.db");
    if !path.exists() {
        bail!(
            "no database at {} (start the server once, or point --data-dir / BBS_DATA_DIR at its data directory)",
            path.display()
        );
    }
    let db = Db::open(&path).with_context(|| format!("opening {}", path.display()))?;
    run(&db, cli.command)
}
