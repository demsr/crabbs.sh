//! Server-side handling of private mail: permission checks, validation,
//! rate limiting, delivery and the live "new mail" notice.

use std::sync::Arc;

use crate::boards::live_account;
use crate::content;
use crate::db::{Account, DbError, Folder, MailError};
use crate::state::{Event, Identity, Shared, Subject};
use crate::ui::Response;

const INTERNAL_ERROR: &str = "Internal error, please try again later.";

/// Broadcast to every session when mail is delivered; the recipient's
/// sessions pick it up and show "New mail from ...".
#[derive(Clone, Debug)]
pub struct MailNotice {
    pub to_user_id: i64,
    pub from: String,
}

/// Mail is for registered, non-suspended users. Returns their live account.
async fn member(shared: &Arc<Shared>, identity: &Identity) -> Result<Account, String> {
    live_account(shared, identity).await.map_err(|e| {
        // live_account's guest message talks about "doing that".
        if e.starts_with("Guests") {
            "Mail is for registered users. Register an account first.".to_string()
        } else {
            e
        }
    })
}

pub async fn unread_count(shared: &Arc<Shared>, user_id: i64) -> i64 {
    shared
        .blocking(move |s| s.db.unread_mail_count(user_id))
        .await
        .unwrap_or(0)
}

pub async fn open_mailbox(
    shared: &Arc<Shared>,
    identity: &Identity,
    folder: Folder,
    notice: Option<String>,
) -> Response {
    let account = match member(shared, identity).await {
        Ok(a) => a,
        Err(e) => return Response::Error(e),
    };
    let id = account.id;
    let result = shared
        .blocking(move |s| Ok::<_, DbError>((s.db.list_folder(id, folder)?, s.db.unread_mail_count(id)?)))
        .await;
    match result {
        Ok((items, unread)) => Response::Mailbox {
            folder,
            items,
            unread,
            notice,
        },
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

pub async fn read_message(shared: &Arc<Shared>, identity: &Identity, message_id: i64) -> Response {
    let account = match member(shared, identity).await {
        Ok(a) => a,
        Err(e) => return Response::Error(e),
    };
    let id = account.id;
    let result = shared
        .blocking(move |s| {
            // Opening marks it read; the count is taken afterwards so the
            // menu reflects that.
            let message = s.db.read_message(id, message_id)?;
            Ok::<_, DbError>((message, s.db.unread_mail_count(id)?))
        })
        .await;
    match result {
        Ok((Some(message), unread)) => Response::Message { message, unread },
        Ok((None, _)) => Response::Error("That message doesn't exist any more.".into()),
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

pub async fn send(
    shared: &Arc<Shared>,
    identity: &Identity,
    to: String,
    subject: String,
    body: String,
) -> Response {
    let sender = match member(shared, identity).await {
        Ok(a) => a,
        Err(e) => return Response::MailFailed(e),
    };
    let (subject, body) = match (content::clean_subject(&subject), content::clean_body(&body)) {
        (Ok(s), Ok(b)) => (s, b),
        (Err(e), _) | (_, Err(e)) => return Response::MailFailed(e),
    };

    let name = to.trim().to_string();
    let lookup = name.clone();
    let recipient = match shared.blocking(move |s| s.db.account_by_name(&lookup)).await {
        Ok(Some(account)) => account,
        Ok(None) => return Response::MailFailed(format!("There is no user called \"{name}\".")),
        Err(_) => return Response::MailFailed(INTERNAL_ERROR.into()),
    };

    let subject_limit = Subject::User(sender.id);
    if !shared.limiter.allowed(subject_limit, Event::MailSend) {
        return Response::MailFailed("You're sending mail too fast. Please wait a few minutes.".into());
    }
    shared.limiter.record(subject_limit, Event::MailSend);

    let (sender_id, recipient_id) = (sender.id, recipient.id);
    let result = shared
        .blocking(move |s| s.db.send_message(sender_id, recipient_id, &subject, &body))
        .await;
    match result {
        Ok(_) => {
            if sender.id != recipient.id {
                // Nobody listening is fine: it will be in their inbox.
                let _ = shared.mail.send(MailNotice {
                    to_user_id: recipient.id,
                    from: sender.username.clone(),
                });
            }
            open_mailbox(
                shared,
                identity,
                Folder::Inbox,
                Some(format!("Message sent to {}.", recipient.username)),
            )
            .await
        }
        Err(MailError::Blocked) => {
            Response::MailFailed("You can't send messages to that user.".into())
        }
        Err(MailError::RecipientFull) => {
            Response::MailFailed(format!("{}'s mailbox is full.", recipient.username))
        }
        Err(MailError::SentFull) => Response::MailFailed(
            "Your sent folder is full. Delete some old messages first.".into(),
        ),
        Err(MailError::TooFast) => Response::MailFailed(format!(
            "You've sent {} several messages recently. Please wait a few minutes.",
            recipient.username
        )),
        Err(MailError::Db(_)) => Response::MailFailed(INTERNAL_ERROR.into()),
    }
}

pub async fn delete_message(
    shared: &Arc<Shared>,
    identity: &Identity,
    message_id: i64,
    folder: Folder,
) -> Response {
    let account = match member(shared, identity).await {
        Ok(a) => a,
        Err(e) => return Response::Error(e),
    };
    let id = account.id;
    match shared.blocking(move |s| s.db.delete_message(id, message_id)).await {
        Ok(()) => open_mailbox(shared, identity, folder, Some("Message deleted.".into())).await,
        Err(DbError::NotFound) => Response::Error("That message doesn't exist any more.".into()),
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

pub async fn list_blocks(shared: &Arc<Shared>, identity: &Identity, notice: Option<String>) -> Response {
    let account = match member(shared, identity).await {
        Ok(a) => a,
        Err(e) => return Response::Error(e),
    };
    let id = account.id;
    match shared.blocking(move |s| s.db.list_blocks(id)).await {
        Ok(names) => Response::Blocks { names, notice },
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

/// Blocks a user; returns to the inbox.
pub async fn block(shared: &Arc<Shared>, identity: &Identity, name: String) -> Response {
    let me = match member(shared, identity).await {
        Ok(a) => a,
        Err(e) => return Response::Error(e),
    };
    let lookup = name.clone();
    let target = match shared.blocking(move |s| s.db.account_by_name(&lookup)).await {
        Ok(Some(account)) => account,
        Ok(None) => return Response::Error(format!("There is no user called \"{name}\".")),
        Err(_) => return Response::Error(INTERNAL_ERROR.into()),
    };
    if target.id == me.id {
        return Response::Error("You can't block yourself.".into());
    }
    let (me_id, target_id) = (me.id, target.id);
    match shared.blocking(move |s| s.db.add_block(me_id, target_id)).await {
        Ok(()) => {
            open_mailbox(
                shared,
                identity,
                Folder::Inbox,
                Some(format!(
                    "Blocked {}. They can no longer send you mail (see B in the mailbox).",
                    target.username
                )),
            )
            .await
        }
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}

pub async fn unblock(shared: &Arc<Shared>, identity: &Identity, name: String) -> Response {
    let me = match member(shared, identity).await {
        Ok(a) => a,
        Err(e) => return Response::Error(e),
    };
    let lookup = name.clone();
    let target = match shared.blocking(move |s| s.db.account_by_name(&lookup)).await {
        Ok(Some(account)) => account,
        Ok(None) => return Response::Error(format!("There is no user called \"{name}\".")),
        Err(_) => return Response::Error(INTERNAL_ERROR.into()),
    };
    let (me_id, target_id) = (me.id, target.id);
    match shared
        .blocking(move |s| s.db.remove_block(me_id, target_id))
        .await
    {
        Ok(()) => list_blocks(shared, identity, Some(format!("Unblocked {}.", target.username))).await,
        Err(_) => Response::Error(INTERNAL_ERROR.into()),
    }
}
