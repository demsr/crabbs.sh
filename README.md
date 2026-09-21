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

**Changing your password:** main menu → **Change password**. You must enter
your current password, then the new one twice. The new password follows the
same rules and must differ from the old one. On success your other sessions
are signed out (so anyone holding the old password or a stale session is
locked out); the session you changed it from stays. Wrong current-password
attempts are limited to 5 per 10 minutes per user, so a hijacked session
can't be used to guess it. Your SSH keys are not affected; remove keys you no
longer trust in the **SSH keys** screen.

**Forgotten password:** there is no self-service recovery (no email is
stored). A sysop with shell access resets it with `bbsadmin user passwd`.

## Message boards

Main menu → **Message boards**. Boards contain threads, threads contain posts.
Everyone (including guests) can read; only registered users can post.
Three boards (General, Tech, Off-Topic) are created on first start; manage
boards with the admin tool (below).

- Board list / thread list: ↑/↓, Enter to open, Esc to go back
- Thread list: `n` starts a new thread, `m` marks the whole board as read,
  ←/→ (or `,` `.`) change page
- Thread view: ↑/↓ scroll, `r` to reply, Esc back (to the list page you came
  from); Space/PgDn reads on, ←/→ change page, `g`/`G` first/last page
- Editor: type normally (lines word-wrap at 78 columns), **Ctrl-D** posts,
  Esc cancels, Tab indents (4 spaces), Shift-Tab jumps back to the title,
  and Enter in the title moves to the message. Spacing and indentation are
  preserved, so ASCII art and code blocks work (lines up to 78 columns fit
  an 80-column terminal)

**Paging.** Thread lists show 25 threads per page (most recently active
first, the title says `page 2/5 · 123 threads`) and threads show 25 posts per
page. Posts are numbered across the whole thread (`#26` is the first post on
page 2). PgDn at the last row of a list, or Space/PgDn at the bottom of a
thread page, simply continues onto the next page; PgUp at the top goes back
to the bottom of the previous one. Back from a thread returns to the list page
you opened it from, and replying opens the last page at the newest post.

Limits: titles 3-80 characters, messages up to 2000 characters, 2000 posts per
thread. Posting is limited to 10 posts per 10 minutes and 3 new threads per hour per user.
All text is sanitised on the server (control characters and bidi/zero-width
characters removed) regardless of what the client sent. Whitespace is kept
as typed, apart from trailing spaces, tabs (expanded to 4 spaces), runs of
more than 3 blank lines and blank lines at the start or end of a post.

### Unread markers

For registered users the BBS remembers how far each thread has been read:

- the main menu shows `Message boards (N unread)`, the number of threads with
  something new;
- the board list shows `N unread` per board;
- the thread list marks threads with unread posts with `*` and `[N new]`,
  which also catches new replies in old threads, not just new topics;
- opening a thread goes to the page containing the first unread post,
  scrolls to it, tags unread posts `NEW` and marks the thread read **up to
  the end of the page shown** - posts on pages you haven't reached stay
  unread, so `[N new]` keeps counting them.

Your own posts are never unread for you, and history from before you
registered counts as read. Guests have no markers. State lives in the
`thread_reads` table (one pointer per user and thread) and is created
automatically in existing databases; for existing users, everything posted
since they registered shows as unread once.

## Chat

Main menu → **Chat** is one public room, live: what people type appears on
everyone else's screen immediately, without them pressing anything. The
right-hand panel shows who is in the room (hidden on terminals narrower than
60 columns), and the log shows joins, leaves, messages and `/me` actions.
Times are UTC.

- Enter sends, Esc (or `/quit`) leaves; PgUp/PgDn or ↑/↓ scroll back, End
  jumps to the newest line. While scrolled back the view stays put when new
  lines arrive.
- `/me <action>`, `/who`, `/help`. Commands and errors like "Slow down" are
  shown only to you.
- Only registered users can join; guests are told to register first.
- New arrivals see the last 100 events. History lives in memory only, so it
  is empty after a server restart.
- Messages are single lines of up to 300 characters, cleaned of control and
  bidi/zero-width characters on the server, and limited to 8 per 10 seconds
  per user. A suspended user can't speak (and is disconnected shortly after).
- Someone connected twice is listed once and announced once.

How it works: the room (`src/chat.rs`) numbers every event and broadcasts it;
each session has a small listener task that redraws its screen when an event
arrives. A joining session gets a snapshot stamped with a sequence number and
ignores older broadcast events, so nothing is shown twice or lost, and a
listener that falls too far behind resynchronises from a fresh snapshot.

## Mail

Main menu → **Mail**: private messages between registered users, delivered
even if the recipient is offline. The main menu shows `Mail (N unread)`, and
a user who is connected is told immediately ("New mail from alice.") without
pressing anything.

- Inbox / Sent (Tab switches). Enter reads, `c` composes, `d` deletes, `r`
  refreshes, `B` opens the list of blocked users.
- Composing: To (a username, any capitalisation), Subject, Message. Ctrl-D
  sends, Esc cancels, Tab moves between fields. The message follows the same
  rules as posts (up to 2000 characters, spacing and indentation kept).
- Reading a message marks it read. `r` replies (recipient and "Re:" subject
  prefilled), `d` deletes, `b` blocks the sender.
- Each side deletes only its own copy; a message disappears from the database
  once both have deleted it.
- **Blocking:** a blocked user's mail is refused ("You can't send messages to
  that user"). Blocking is one-way and reversible from the blocked-users list.

Limits, because open registration plus private messages invites abuse: at
most 10 messages per 10 minutes per sender, 5 per 10 minutes to any one
recipient, and 200 messages per folder (a full inbox refuses new mail, a full
sent folder has to be cleaned up before sending more). Guests and suspended
accounts can't use mail.

**Privacy:** messages are stored unencrypted in `bbs.db`, so anyone with
access to the database file can read them. The admin tool deliberately has no
command to read mail, but it is not a security boundary against the person
running the server.

## Who's online

Main menu → **Who's online** lists the registered users currently connected,
what each is doing (main menu, reading a thread, writing a post, chatting, reading mail, ...) and
for how long, with a `[sysop]` badge for sysops. Guests are only counted, not
named. `r` refreshes the snapshot.

## Sysops and administration

Administration is split in two, on purpose:

- **Inside the BBS** a sysop can moderate content in context: `x` in a
  thread list deletes the selected thread (after confirmation), `x` in a
  thread asks for a post number (the thread-wide `#` shown on the post, which
  must be on the page you're viewing) and deletes that post; you stay on the
  same page. Sysop posts carry a `[sysop]` badge. Everything else stays out of the public SSH interface.
- **`bbsadmin`** is a command-line tool for everything else, run on the
  host with shell access. It works directly on the SQLite database using the
  same code as the server, and can run while the server is up; changes apply
  immediately.

```
cargo run --bin bbsadmin -- <command>        # or ./target/release/bbsadmin
```

It finds the database through `--data-dir` or `BBS_DATA_DIR` (default
`data`), and refuses to run if there is no `bbs.db` there.

| Command | What it does |
|---|---|
| `stats` | counts of users, boards, threads, posts, keys |
| `board list` / `add NAME [-d TEXT]` / `rename ID NAME` / `describe ID TEXT` / `move ID POS` | manage boards |
| `board delete ID [--force]` | delete a board; refuses if it has threads unless `--force` |
| `user list` / `show NAME` | list users; show one with their SSH keys |
| `user add NAME [--sysop]` | create an account (asks for a password without echo; `--password-stdin` for scripts) |
| `user promote NAME` / `demote NAME` | grant or remove the sysop role |
| `user ban NAME [-r REASON]` / `unban NAME` | suspend or restore an account |
| `user passwd NAME` | set a new password, e.g. for someone who forgot theirs (users can change their own from the menu) |
| `user remove-key KEY_ID` | remove an SSH key (ids are shown by `user show`) |
| `thread list BOARD_ID` / `delete ID` | list or delete threads |
| `post list THREAD_ID` / `delete ID` | list posts with ids, or delete one (the thread goes with its last post) |

**Creating the first sysop:** either `bbsadmin user add sysop --sysop`, or
register normally in the BBS and then `bbsadmin user promote yourname`.

A **ban** blocks login (password and key) and posting immediately, and a
connected user is disconnected within about 10 seconds. Roles are never
trusted from the session: every moderation action and every post re-reads the
account from the database, so a demotion or ban takes effect at once.

## Security notes

- Passwords are hashed with Argon2id (random salt); nothing is stored in
  plaintext. At most 4 hashes run concurrently to bound memory use.
- Unknown usernames take as long to reject as wrong passwords.
- Per-address limits (in memory, IPv6 grouped by /64): 8 concurrent
  connections, 10 failed logins per 10 minutes, 3 registrations per hour.
  Behind a reverse proxy / NAT all users share one address, so tune
  `src/state.rs` accordingly.
- Chat is registered-users only, rate limited per user, sanitised on the
  server, and checks for suspension on every message.
- Mail has per-sender and per-recipient rate limits, mailbox size caps and a
  block list; every action re-checks the account in the database.
- Only `password` and `publickey` authentication are offered. Exec,
  subsystem (sftp) and port-forwarding requests are refused.
- Bans and roles are enforced from the database on every action, not from
  the login session (see above). The admin tool is not reachable over the
  network; protect the data directory (`bbs.db`, `host_key`) with normal
  file permissions.
- Not done yet: pre-auth connection timeout (an idle unauthenticated
  connection is only dropped after the 1h inactivity timeout, bounded by the
  per-address connection limit), and self-service recovery of a forgotten
  password (an admin resets it; see above).

## Controls

- Menu: ↑/↓ or `j`/`k`, Enter, or a number key; `q` logs off
- Forms: Tab/Enter next field, Esc cancel
- Ctrl-C logs off from anywhere
- Shift+Enter (and Alt/Ctrl+Enter) count as a plain Enter, however your
  terminal encodes them (ESC CR, kitty `CSI 13;2u`, xterm modifyOtherKeys).
  Other Alt+key combinations are ignored. A lone Esc press is still Esc.

## Layout

- `src/lib.rs` — the library everything lives in; two binaries use it
- `src/main.rs` — the SSH server: configuration, host key, DB and start-up
- `src/bin/bbsadmin.rs` — the admin command-line tool
- `tests/admin_cli.rs` — runs the real `bbsadmin` against a temporary database
- `src/server.rs` — `russh` handler: authentication (password + public
  key), session/PTY setup, and executing the UI's `Request`s
- `src/state.rs` — state shared by all connections (DB, rate limiter,
  who's-online registry, session `Identity`)
- `src/db.rs` — SQLite schema, migrations and queries (`users`, `ssh_keys`,
  `boards`, `threads`, `posts`, `thread_reads`, `messages`, `blocks`),
  including everything the admin tool uses
- `src/boards.rs` — board requests: permission checks, validation, rate
  limiting; calls into `db.rs`
- `src/chat.rs` — the chat room: membership, history, sequenced broadcast
- `src/mail.rs` — mail requests: permissions, validation, limits, delivery
  notices
- `src/content.rs` — sanitising and limits for titles, post bodies and chat
  lines
- `src/auth.rs` — username/password validation, Argon2, public-key parsing
- `src/terminal.rs` — adapts an SSH channel into an `io::Write` sink
- `src/ui/` — the terminal UI. `App` in `mod.rs` is a small screen state
  machine; it is synchronous and asks the server to do async work by
  returning a `Request`, then receives a `Response`. `input.rs` parses raw
  bytes into keys and has the text-field widget; `register.rs` and
  `keys.rs`, `boards.rs` (board list, thread list, thread view),
  `compose.rs` (multi-line editor and compose screen), `online.rs`,
  `chat.rs` (the chat screen), `password.rs` and `mail.rs` (mailbox, message
  view, compose, blocked list) are screens.

### Adding a screen

1. Add a variant to `Screen` (and a `MenuItem` if it's reachable from the menu).
2. Give it `handle(Key) -> Event` and `draw(frame, area)` methods.
3. If it needs the database, add a `Request`/`Response` pair and handle the
   request in `BbsHandler::serve`.
4. If it needs to update while the user is idle (like chat or new-mail
   notices), the session's screen state is shared with a background listener
   through a mutex in `server.rs`; never hold that lock across an `.await`.
   Add a broadcast channel to `Shared` and a branch in `spawn_listener`.

## Next steps

- Chat rooms/channels
