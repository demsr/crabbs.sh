//! Server-side handling of sysop-only, non-content-specific requests
//! (currently just the message of the day). Board and post moderation live
//! in `boards.rs`, next to the content they act on.

use std::sync::Arc;

use crate::boards::require_sysop;
use crate::content;
use crate::state::{Identity, Shared};
use crate::ui::Response;

const INTERNAL_ERROR: &str = "Internal error, please try again later.";

pub async fn get_motd(shared: &Arc<Shared>, identity: &Identity) -> Response {
    fetch_motd(shared, identity, None).await
}

pub async fn set_motd(shared: &Arc<Shared>, identity: &Identity, text: String) -> Response {
    let Some(user_id) = identity.user_id() else {
        return Response::SysopActionFailed("Only sysops can do that.".into());
    };
    if let Err(e) = require_sysop(shared, identity).await {
        return Response::SysopActionFailed(e);
    }
    let text = match content::clean_motd(&text) {
        Ok(text) => text,
        Err(e) => return Response::SysopActionFailed(e),
    };
    let notice = if text.is_empty() {
        "MOTD cleared."
    } else {
        "MOTD saved."
    };
    match shared.blocking(move |s| s.db.set_motd(&text, user_id)).await {
        Ok(()) => fetch_motd(shared, identity, Some(notice.into())).await,
        Err(_) => Response::SysopActionFailed(INTERNAL_ERROR.into()),
    }
}

async fn fetch_motd(shared: &Arc<Shared>, identity: &Identity, notice: Option<String>) -> Response {
    if let Err(e) = require_sysop(shared, identity).await {
        return Response::SysopActionFailed(e);
    }
    match shared.blocking(|s| s.db.get_motd()).await {
        Ok(text) => Response::Motd { text, notice },
        Err(_) => Response::SysopActionFailed(INTERNAL_ERROR.into()),
    }
}
