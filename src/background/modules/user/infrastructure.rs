use std::{
    path::PathBuf,
    sync::{Arc, Once},
};

use parking_lot::Mutex;
use seelen_core::{
    handlers::SeelenEvent,
    system_state::{FolderChangedArgs, FolderType, User},
};

use crate::{app::emit_to_webviews, state::application::FULL_STATE, trace_lock};

use super::application::{UserManager, UserManagerEvent};

fn maybe_redact_user(mut user: User) -> User {
    if FULL_STATE.load().settings.streaming_mode {
        user.email = Some("***@seelen.io".to_string());
    }
    user
}

fn register_user_events() {
    static TAURI_EVENT_REGISTRATION: Once = Once::new();
    TAURI_EVENT_REGISTRATION.call_once(|| {
        UserManager::subscribe(|event| match event {
            UserManagerEvent::UserUpdated => {
                let guard = trace_lock!(UserManager::instance());
                emit_to_webviews(
                    SeelenEvent::UserChanged,
                    maybe_redact_user(guard.user.clone()),
                );
            }
            UserManagerEvent::FolderChanged(folder) => {
                emit_to_webviews(
                    SeelenEvent::UserFolderChanged,
                    FolderChangedArgs {
                        of_folder: folder,
                        content: get_user_folder_content(folder),
                    },
                );
            }
        });
    });
}

fn get_user_manager() -> &'static Arc<Mutex<UserManager>> {
    register_user_events();
    UserManager::instance()
}

/// Returns false and kicks off the initialization out of the caller thread when the
/// manager isn't built yet, see [`UserManager::init_in_background`].
fn ensure_user_manager() -> bool {
    if UserManager::is_initialized() {
        return true;
    }
    register_user_events();
    UserManager::init_in_background();
    false
}

pub fn reemit_user() {
    if !ensure_user_manager() {
        // the background initialization will emit the user once it finishes
        return;
    }
    let user = trace_lock!(get_user_manager()).user.clone();
    emit_to_webviews(SeelenEvent::UserChanged, maybe_redact_user(user));
}

#[tauri::command(async)]
pub fn get_user() -> User {
    // never block the calling widget on the initialization, `SeelenEvent::UserChanged`
    // is emitted with the real user as soon as it is available
    if !ensure_user_manager() {
        return maybe_redact_user(UserManager::unknown_user());
    }
    maybe_redact_user(trace_lock!(get_user_manager()).user.clone())
}

#[tauri::command(async)]
pub fn get_user_folder_content(folder_type: FolderType) -> Vec<PathBuf> {
    // same as `get_user`, `SeelenEvent::UserFolderChanged` will bring the real content
    if !ensure_user_manager() {
        return Vec::new();
    }
    let manager = trace_lock!(get_user_manager());
    match manager.folders.get(&folder_type) {
        Some(details) => details.content.clone(),
        None => Vec::new(),
    }
}
