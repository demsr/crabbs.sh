# rust-bbs

A small SSH-accessible BBS written in Rust, using `russh` for the SSH
server, `ratatui` for the terminal UI (rendered live over the SSH
connection) and SQLite for storage.

## Run it

```
cargo run
```

Starts an SSH server on port 2222. Connect with:

```
ssh -p 2222 guest@localhost
```

### Configuration (environment variables)

| Variable             | Default   | Meaning                                        |
|----------------------|-----------|------------------------------------------------|
| `BBS_PORT`           | `2222`    | TCP port to listen on                          |
| `BBS_DATA_DIR`       | `data`    | Holds `bbs.db` (SQLite) and `host_key`         |
| `BBS_GUEST_PASSWORD` | `letmein` | Password of the public `guest` account         |

**Change `BBS_GUEST_PASSWORD` before exposing the server publicly** if you
don't want everyone who reads this README to get in. The guest account only
exists so people can reach the registration screen.

The host key is generated on first start and stored in `data/host_key`
(mode 0600), so clients don't get host-key-changed warnings after a restart.
Back it up along with `data/bbs.db`.

## Accounts

1. Connect as `guest` and choose **Register an account** from the menu.
2. Pick a username and password. You are switched to your new account
   immediately, without reconnecting.
3. Next time connect directly: `ssh -p 2222 yourname@host`.
4. Optionally, open **SSH keys** and paste your public key
   (`~/.ssh/id_ed25519.pub`). After that you can log in with the key instead
   of the password.

Usernames: 3-16 characters, letters/digits/`_`/`-`, starting with a letter,
case-insensitive. Passwords: 8-128 characters.

## Message boards

Main menu → **Message boards**. Boards contain threads, threads contain posts.
Everyone (including guests) can read; only registered users can post.
Three boards (General, Tech, Off-Topic) are created on first start; further
boards can be added by inserting into the `boards` table for now.

- Board list / thread list: ↑/↓, Enter to open, Esc to go back
- Thread list: `n` starts a new thread
- Thread view: ↑/↓, PgUp/PgDn or Space to scroll, `r` to reply, Esc back
- Editor: type normally (lines word-wrap at 76 columns), **Ctrl-D** posts,
  Esc cancels, Tab switches between title and message

Limits: titles 3-80 characters, messages up to 2000 characters, 200 posts per
thread, 100 threads listed per board (most recently active first). Posting is
limited to 10 posts per 10 minutes and 3 new threads per hour per user.
All text is sanitised on the server (control characters and bidi/zero-width
characters removed) regardless of what the client sent.

## Security notes

- Passwords are hashed with Argon2id (random salt); nothing is stored in
  plaintext. At most 4 hashes run concurrently to bound memory use.
- Unknown usernames take as long to reject as wrong passwords.
- Per-address limits (in memory, IPv6 grouped by /64): 8 concurrent
  connections, 10 failed logins per 10 minutes, 3 registrations per hour.
  Behind a reverse proxy / NAT all users share one address, so tune
  `src/state.rs` accordingly.
- Only `password` and `publickey` authentication are offered. Exec,
  subsystem (sftp) and port-forwarding requests are refused.
- Not done yet: pre-auth connection timeout (an idle unauthenticated
  connection is only dropped after the 1h inactivity timeout, bounded by the
  per-address connection limit), account recovery, password change.

## Controls

- Menu: ↑/↓ or `j`/`k`, Enter, or a number key; `q` logs off
- Forms: Tab/Enter next field, Esc cancel
- Ctrl-C logs off from anywhere

## Layout

- `src/main.rs` — configuration, host key, DB and server start-up
- `src/server.rs` — `russh` handler: authentication (password + public
  key), session/PTY setup, and executing the UI's `Request`s
- `src/state.rs` — state shared by all connections (DB, rate limiter,
  session `Identity`)
- `src/db.rs` — SQLite schema and queries (`users`, `ssh_keys`, `boards`,
  `threads`, `posts`)
- `src/boards.rs` — board requests: permission checks, validation, rate
  limiting; calls into `db.rs`
- `src/content.rs` — sanitising and limits for titles and post bodies
- `src/auth.rs` — username/password validation, Argon2, public-key parsing
- `src/terminal.rs` — adapts an SSH channel into an `io::Write` sink
- `src/ui/` — the terminal UI. `App` in `mod.rs` is a small screen state
  machine; it is synchronous and asks the server to do async work by
  returning a `Request`, then receives a `Response`. `input.rs` parses raw
  bytes into keys and has the text-field widget; `register.rs` and
  `keys.rs`, `boards.rs` (board list, thread list, thread view) and
  `compose.rs` (multi-line editor and compose screen) are screens.

### Adding a screen

1. Add a variant to `Screen` (and a `MenuItem` if it's reachable from the menu).
2. Give it `handle(Key) -> Event` and `draw(frame, area)` methods.
3. If it needs the database, add a `Request`/`Response` pair and handle the
   request in `BbsHandler::serve`.

## Next steps

- Sysop role: create/delete boards, delete posts, ban users
- Unread tracking and new-post indicators
- Thread/post pagination beyond the current caps
- Shared online-users registry for "Who's online" (and later live chat)
- Password change, sysop/moderation tools
