use notify_debouncer_full::{
    DebounceEventResult, Debouncer, FileIdMap, new_debouncer,
    notify::{ReadDirectoryChangesWatcher, RecursiveMode},
};
use parking_lot::Mutex;
use seelen_core::system_state::{FolderType, User};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, LazyLock, Once,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::Manager;
use windows::Win32::{
    Security::Authentication::Identity::{NameDisplay, NameSamCompatible},
    System::SystemInformation::ComputerNameDnsDomain,
};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
};

use crate::{
    app::get_app_handle,
    error::{Result, ResultLogExt},
    event_manager, trace_lock,
    windows_api::WindowsApi,
};

use super::domain::PictureQuality;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserManagerEvent {
    UserUpdated,
    FolderChanged(FolderType),
}

/// Set once the first (potentially slow) initialization of the manager has finished.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub struct FolderDetails {
    pub path: PathBuf,
    pub content: Vec<PathBuf>,
    /// `None` when the folder could not be watched (missing path, denied access, etc).
    /// The already scanned content is still served in that case.
    _watcher: Option<Debouncer<ReadDirectoryChangesWatcher, FileIdMap>>,
}

#[derive(Debug)]
pub struct UserManager {
    pub user: User,
    pub folders: HashMap<FolderType, FolderDetails>,
}

unsafe impl Send for UserManager {}
unsafe impl Send for UserManagerEvent {}

event_manager!(UserManager, UserManagerEvent);

impl UserManager {
    /// Blocks until the manager is built, prefer [`Self::is_initialized`] + this, or
    /// [`Self::init_in_background`], on paths that shouldn't wait (like IPC commands).
    pub fn instance() -> &'static Arc<Mutex<Self>> {
        static USER_MANAGER: LazyLock<Arc<Mutex<UserManager>>> = LazyLock::new(|| {
            let manager = Arc::new(Mutex::new(UserManager::new()));
            INITIALIZED.store(true, Ordering::Release);
            manager
        });
        &USER_MANAGER
    }

    /// Building the manager scans and starts watching all the known user folders, which
    /// on some systems takes a long time (huge trees, slow/unavailable paths, domain
    /// lookups on `get_logged_user`). Doing that lazily from an IPC command left widgets
    /// like the power menu and the apps menu waiting forever on `get_user`, so the work
    /// is done once on a background thread instead.
    pub fn init_in_background() {
        static SPAWNED: Once = Once::new();
        SPAWNED.call_once(|| {
            std::thread::spawn(|| {
                let _ = Self::instance();
                // widgets that got a placeholder while this was running are updated by these
                Self::send(UserManagerEvent::UserUpdated);
                for &folder_type in FolderType::values() {
                    Self::send(UserManagerEvent::FolderChanged(folder_type));
                }
            });
        });
    }

    pub fn is_initialized() -> bool {
        INITIALIZED.load(Ordering::Acquire)
    }

    /// Placeholder served while [`Self::init_in_background`] is still running.
    pub fn unknown_user() -> User {
        User {
            name: "???".to_string(),
            domain: String::new(),
            profile_home_path: PathBuf::new(),
            email: None,
            one_drive_path: None,
            profile_picture_path: None,
            xbox_gamertag: None,
        }
    }

    fn get_path_from_folder(folder_type: &FolderType) -> Option<PathBuf> {
        let resolver = get_app_handle().path();
        match folder_type {
            FolderType::Recent => {
                Some(resolver.data_dir().ok()?.join("Microsoft\\Windows\\Recent"))
            }
            FolderType::Desktop => resolver.desktop_dir().ok(),
            FolderType::Downloads => resolver.download_dir().ok(),
            FolderType::Documents => resolver.document_dir().ok(),
            FolderType::Pictures => resolver.picture_dir().ok(),
            FolderType::Videos => resolver.video_dir().ok(),
            FolderType::Music => resolver.audio_dir().ok(),
        }
    }

    fn get_logged_on_user_sid() -> Result<String> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let settings = hklm
            .open_subkey("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Authentication\\LogonUI")?;
        Ok(settings.get_value("LastLoggedOnUserSID")?)
    }

    fn get_folder_content(base_path: PathBuf, folder_type: FolderType) -> Result<Vec<PathBuf>> {
        // Recent is a flat folder of .lnk shortcuts; others are scanned up to 5 levels deep
        // to avoid exploding on large/deep folder trees.
        let max_depth = match folder_type {
            FolderType::Recent => 1,
            _ => 5,
        };

        let mut list = Vec::new();

        for entry in walkdir::WalkDir::new(&base_path)
            .follow_links(false)
            .max_depth(max_depth)
            .into_iter()
            .filter_entry(|e| !is_ignored_entry(e.path()))
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() || is_ignored_file(path) {
                continue;
            }

            list.push(path.to_path_buf());
        }

        Ok(list)
    }

    /// Tries every known picture quality from highest to lowest, since Windows only
    /// populates the registry values it actually generated: Windows 11 writes `Image1080`,
    /// but Windows 10 stops at `Image448`, so requesting `Image1080` there always fails.
    fn get_user_profile_picture_path(sid: &str) -> Result<PathBuf> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let settings = hklm.open_subkey(
            format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\AccountPicture\\Users\\{sid}")
                .as_str(),
        )?;

        for quality in PictureQuality::ALL {
            if let Ok(path) = settings.get_value::<String, _>(quality.as_str()) {
                let path = PathBuf::from(path);
                if path.exists() {
                    return Ok(path);
                }
            }
        }

        Err("No profile picture found for any known quality".into())
    }

    fn get_one_drive_attributes() -> Result<(String, PathBuf)> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let settings = hkcu.open_subkey("SOFTWARE\\Microsoft\\OneDrive\\Accounts\\Personal")?;
        let email: String = settings.get_value("UserEmail")?;
        let path: String = settings.get_value("UserFolder")?;
        Ok((email, PathBuf::from(path)))
    }

    fn get_xbox_gamertag() -> Result<String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let settings = hkcu.open_subkey("SOFTWARE\\Microsoft\\XboxLive")?;
        let gamertag: String = settings.get_value("Gamertag")?;
        Ok(gamertag)
    }

    fn get_logged_user() -> User {
        let domain = WindowsApi::get_computer_name(ComputerNameDnsDomain).unwrap_or_default();
        let name = WindowsApi::get_username(NameDisplay)
            .or_else(|_| -> Result<String> {
                // A legacy account name (for example, Engineering\JSmith).
                // The domain-only version includes trailing backslashes (\).
                let name = WindowsApi::get_username(NameSamCompatible)?;
                let name = name.split("\\").last().unwrap_or_default();
                match name.is_empty() {
                    true => Err("Empty username".into()),
                    false => Ok(name.to_string()),
                }
            })
            .unwrap_or_else(|_| "???".to_string()); // no username

        let mut user = User {
            name,
            domain,
            profile_home_path: PathBuf::new(), // deprecated, remove this is unnecessary
            email: None,
            one_drive_path: None,
            profile_picture_path: None,
            xbox_gamertag: None,
        };

        if let Ok(sid) = Self::get_logged_on_user_sid() {
            user.profile_picture_path = Self::get_user_profile_picture_path(&sid).ok();
            if let Ok((user_mail, one_drive_path)) = Self::get_one_drive_attributes() {
                user.email = Some(user_mail);
                user.one_drive_path = Some(one_drive_path);
            }
        }

        user.xbox_gamertag = Self::get_xbox_gamertag().ok();

        user
    }

    fn create_folder_watcher(
        path: &PathBuf,
        folder_type: FolderType,
    ) -> Result<Debouncer<ReadDirectoryChangesWatcher, FileIdMap>> {
        let mut debouncer = new_debouncer(
            Duration::from_millis(1000),
            None,
            move |result: DebounceEventResult| match result {
                Ok(_events) => {
                    Self::reload_folder_content(folder_type).log_error();
                }
                Err(errors) => {
                    log::error!("Folder Watcher Error for {:?}: {errors:?}", folder_type);
                }
            },
        )?;
        debouncer.watch(path, RecursiveMode::Recursive)?;
        Ok(debouncer)
    }

    fn reload_folder_content(folder_type: FolderType) -> Result<()> {
        let mut manager = trace_lock!(Self::instance());

        if let Some(folder_details) = manager.folders.get_mut(&folder_type) {
            folder_details.content =
                Self::get_folder_content(folder_details.path.clone(), folder_type)?;
            drop(manager);
            let _ = Self::event_tx().send(UserManagerEvent::FolderChanged(folder_type));
        }

        Ok(())
    }

    /// Infallible on purpose: this runs lazily from the `get_user` and
    /// `get_user_folder_content` commands, so panicking here would leave those IPC calls
    /// pending forever and poison the lazy instance for every later call. Folders that
    /// can't be read or watched are skipped or degraded instead.
    pub fn new() -> Self {
        let mut folders = HashMap::new();

        for &folder_type in FolderType::values() {
            let Some(path) = Self::get_path_from_folder(&folder_type) else {
                continue;
            };

            if !path.is_dir() {
                log::warn!("Skipping user folder {folder_type:?}, missing directory: {path:?}");
                continue;
            }

            let content = Self::get_folder_content(path.clone(), folder_type).unwrap_or_default();
            // watching is best effort, a folder that can't be watched is still listed
            let watcher = match Self::create_folder_watcher(&path, folder_type) {
                Ok(watcher) => Some(watcher),
                Err(err) => {
                    log::error!("Failed to watch user folder {folder_type:?} ({path:?}): {err:?}");
                    None
                }
            };

            folders.insert(
                folder_type,
                FolderDetails {
                    path,
                    content,
                    _watcher: watcher,
                },
            );
        }

        Self {
            user: Self::get_logged_user(),
            folders,
        }
    }
}

/// Returns true if the entry should be excluded from the file list.
/// When applied via `filter_entry`, returning true for a directory prunes its entire subtree.
fn is_ignored_entry(path: &std::path::Path) -> bool {
    static IGNORED: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
        HashSet::from([
            // Dev artifact directories
            "node_modules",
            "target",
            "dist",
            "build",
            "out",
            "__pycache__",
            "venv",
            ".venv",
            "env",
            "vendor",
            // VCS
            ".git",
            ".svn",
            ".hg",
            // IDE/editor
            ".idea",
            ".vscode",
            ".next",
            // Cache
            ".cache",
            "cache",
            "Cache",
            // Windows system
            "$RECYCLE.BIN",
            "System Volume Information",
            "WindowsApps",
            "MicrosoftEdgeBackups",
        ])
    });

    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| IGNORED.contains(name))
}

fn is_ignored_file(path: &std::path::Path) -> bool {
    static IGNORED: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
        HashSet::from([
            // Temp / backup
            "ini",
            "dat",
            "bak",
            "tmp",
            "temp",
            "old",
            "swp",
            "save",
            // Crash / memory dumps
            "dmp",
            "blk",
            // In-progress downloads
            "download",
            "crdownload",
            "part",
            // Lock / pid files
            "lock",
            "pid",
            // Log files
            "log",
            // Database / cache files
            "db",
            "sqlite",
            "sqlite3",
            "ldb",
            // Build artifacts
            "obj",
            "pdb",
            "ilk",
            "exp",
            "iobj",
            "ipdb",
            // Windows shortcut noise (internet shortcuts, not file shortcuts)
            "url",
            // System / driver files
            "dll",
            "sys",
            "lib",
            "cat",
            "inf",
            "winmd",
            // Certificates & keys
            "pfx",
            "pem",
            "crl",
            "p7b",
            // Checksums
            "sha512",
            "sha256",
            "sha1",
            "md5",
            // Game / engine packages
            "pak",
            "pck",
            "vdf",
            // iTunes library metadata
            "itdb",
            "itl",
            // Compiled shader cache
            "fxo",
            // Numbered / no-semantic extension (autotools, Python dist)
            "0",
            "1",
            "2",
            "3",
            "4",
            "5",
            "6",
            "7",
            "8",
            "9",
            // Obscure image formats (raw scientific / legacy)
            "pbm",
            "pgm",
            "ppm",
            "ras",
            "sgi",
            "xbm",
            "xpm",
            // Obscure / legacy audio formats
            "8svx",
            "hcom",
            "sndt",
            "voc",
            "spx",
            "aifc",
        ])
    });

    let Some(extension) = path.extension() else {
        return true; // filter files without extension
    };

    let ext = extension.to_string_lossy().to_lowercase();

    // Backup files can have numbers in their name (e.g. 1, 2, 3, etc.)
    if ext.starts_with("bak") || ext.starts_with("backup") {
        return true;
    }

    IGNORED.contains(ext.as_str())
}
