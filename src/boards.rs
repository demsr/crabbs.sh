//! Server-side handling of message board requests: permission checks,
//! validation, rate limiting and database access.

use std::sync::Arc;

use crate::content;
use crate::db::{DbError, MAX_POSTS_PER_THREAD, Role};
use crate::state::{Event, Identity, Shared, Subject};
use crate::ui::{BoardInfo, PostInfo, Response, ThreadDetail, ThreadInfo};

const INTERNAL_ERROR: &str = "Internal error, please try again later.";

pub async fn list_boards(shared: &Arc<Shared>) -> Response {
    match shared.blocking(|s| s.db.list_boards()).await {
        Ok(boards) => Response::Boards(
            boards
                .into_iter()
                .map(|b| BoardInfo {
                    id: b.id,
                    name: b.name,
                    description: b.description,
                    thread_count: b.thread_count,
                })
                .collect(),
        ),
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

pub async fn list_threads(
    shared: &Arc<Shared>,
    board_id: i64,
    notice: Option<String>,
) -> Response {
    let result = shared
        .blocking(move |s| {
            let name = s.db.board_name(board_id)?;
            match name {
                Some(name) => Ok(Some((name, s.db.list_threads(board_id)?))),
                None => Ok(None),
            }
        })
        .await;
    match result {
        Ok(Some((board_name, threads))) => Response::Threads {
            board_id,
            notice,
            board_name,
            threads: threads
                .into_iter()
                .map(|t| ThreadInfo {
                    id: t.id,
                    title: t.title,
                    author: t.author,
                    post_count: t.post_count,
                    last_post: t.last_post,
                })
                .collect(),
        },
        Ok(None) => Response::Error("That board doesn't exist.".into()),
        Err::<_, DbError>(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

pub async fn open_thread(
    shared: &Arc<Shared>,
    thread_id: i64,
    to_end: bool,
    notice: Option<String>,
) -> Response {
    let result = shared
        .blocking(move |s| {
            let Some(head) = s.db.thread_head(thread_id)? else {
                return Ok(None);
            };
            Ok(Some((head, s.db.list_posts(thread_id)?)))
        })
        .await;
    match result {
        Ok(Some((head, posts))) => Response::Thread {
            notice,
            detail: ThreadDetail {
                id: head.id,
                board_id: head.board_id,
                board_name: head.board_name,
                title: head.title,
                posts: posts
                    .into_iter()
                    .map(|p| PostInfo {
                        id: p.id,
                        author: p.author,
                        author_is_sysop: p.author_is_sysop,
                        body: p.body,
                        created: p.created,
                    })
                    .collect(),
            },
            to_end,
        },
        Ok(None) => Response::Error("That thread doesn't exist.".into()),
        Err::<_, DbError>(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

/// Reads the user's current account state; roles and bans can be changed at
/// any time by the admin tool, so this is never taken from the session.
async fn live_account(shared: &Arc<Shared>, identity: &Identity) -> Result<crate::db::Account, String> {
    let Identity::User { id, .. } = identity else {
        return Err("Guests can't do that. Register an account first.".into());
    };
    let id = *id;
    match shared.blocking(move |s| s.db.account(id)).await {
        Ok(Some(account)) if account.banned => Err("Your account has been suspended.".into()),
        Ok(Some(account)) => Ok(account),
        Ok(None) => Err("Your account no longer exists.".into()),
        Err(_) => Err(INTERNAL_ERROR.into()),
    }
}

/// Only sysops may moderate.
async fn require_sysop(shared: &Arc<Shared>, identity: &Identity) -> Result<(), String> {
    match live_account(shared, identity).await? {
        account if account.role == Role::Sysop => Ok(()),
        _ => Err("Only sysops can do that.".into()),
    }
}

/// Only registered, non-suspended users may post, and not too often.
async fn check_can_post(
    shared: &Arc<Shared>,
    identity: &Identity,
    thread_start: bool,
) -> Result<i64, String> {
    let account = live_account(shared, identity)
        .await
        .map_err(|e| e.replace("do that", "post"))?;
    let subject = Subject::User(account.id);
    let limiter = &shared.limiter;
    if !limiter.allowed(subject, Event::Post)
        || (thread_start && !limiter.allowed(subject, Event::NewThread))
    {
        return Err("You're posting too fast. Please wait a few minutes.".into());
    }
    Ok(account.id)
}

fn record_post(shared: &Shared, user_id: i64, thread_start: bool) {
    shared.limiter.record(Subject::User(user_id), Event::Post);
    if thread_start {
        shared
            .limiter
            .record(Subject::User(user_id), Event::NewThread);
    }
}

fn post_error(err: DbError, what: &str) -> Response {
    Response::PostFailed(match err {
        DbError::NotFound => format!("That {what} no longer exists."),
        DbError::LimitReached => {
            format!("This thread is full ({MAX_POSTS_PER_THREAD} posts). Please start a new one.")
        }
        _ => INTERNAL_ERROR.into(),
    })
}

pub async fn create_thread(
    shared: &Arc<Shared>,
    identity: &Identity,
    board_id: i64,
    title: String,
    body: String,
) -> Response {
    let user_id = match check_can_post(shared, identity, true).await {
        Ok(id) => id,
        Err(e) => return Response::PostFailed(e),
    };
    let (title, body) = match (content::clean_title(&title), content::clean_body(&body)) {
        (Ok(t), Ok(b)) => (t, b),
        (Err(e), _) | (_, Err(e)) => return Response::PostFailed(e),
    };
    record_post(shared, user_id, true);

    match shared
        .blocking(move |s| s.db.create_thread(board_id, user_id, &title, &body))
        .await
    {
        Ok(thread_id) => open_thread(shared, thread_id, true, Some("Posted.".into())).await,
        Err(err) => post_error(err, "board"),
    }
}

pub async fn reply(
    shared: &Arc<Shared>,
    identity: &Identity,
    thread_id: i64,
    body: String,
) -> Response {
    let user_id = match check_can_post(shared, identity, false).await {
        Ok(id) => id,
        Err(e) => return Response::PostFailed(e),
    };
    let body = match content::clean_body(&body) {
        Ok(b) => b,
        Err(e) => return Response::PostFailed(e),
    };
    record_post(shared, user_id, false);

    match shared
        .blocking(move |s| s.db.add_post(thread_id, user_id, &body))
        .await
    {
        Ok(()) => open_thread(shared, thread_id, true, Some("Posted.".into())).await,
        Err(err) => post_error(err, "thread"),
    }
}

pub async fn delete_thread(shared: &Arc<Shared>, identity: &Identity, thread_id: i64) -> Response {
    if let Err(e) = require_sysop(shared, identity).await {
        return Response::Error(e);
    }
    match shared.blocking(move |s| s.db.delete_thread(thread_id)).await {
        Ok(board_id) => list_threads(shared, board_id, Some("Thread deleted.".into())).await,
        Err(DbError::NotFound) => Response::Error("That thread no longer exists.".into()),
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

pub async fn delete_post(shared: &Arc<Shared>, identity: &Identity, post_id: i64) -> Response {
    if let Err(e) = require_sysop(shared, identity).await {
        return Response::Error(e);
    }
    match shared.blocking(move |s| s.db.delete_post(post_id)).await {
        Ok(d) if d.thread_deleted => {
            list_threads(
                shared,
                d.board_id,
                Some("Post deleted; the thread is gone too.".into()),
            )
            .await
        }
        Ok(d) => open_thread(shared, d.thread_id, false, Some("Post deleted.".into())).await,
        Err(DbError::NotFound) => Response::Error("That post no longer exists.".into()),
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}
