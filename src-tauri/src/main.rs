#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chain;
mod install;

use install::{
    directory_file_bytes, is_managed_manifest, read_json, write_json, Site, HPATCHZ_NAME, LOGIN_EXPIRED,
    MANIFEST_FILE,
};
#[cfg(test)]
use install::{bytes_to_download, format_bytes, require_free_space};
use psl::{List, Psl};
use reqwest::{
    blocking::{Client, Response},
    Url,
};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::Digest;
use std::{
    collections::HashMap,
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use which::which;
use typed_path::WindowsPath;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;

const LAUNCHER_FILE: &str = "OpenShores-Launcher.exe";
const NEWS_ITEMS: usize = 4;
const LOGIN_NEEDED: &str = "Log in to download and update the game.";
const SESSION_COOKIE: &str = "sessionid";
const LAUNCHER_HEADER: &str = "X-OpenShores-Launcher";
const GAME_EXE: &str = "OpenShores.exe";
const CLIENT_LOG: &str = "ClientInterface.log";
const LOG_READ_CHUNK_SIZE: u64 = 1024 * 1024;
const PATCH_ORIGINALS_DIR: &str = ".openshores-patch-originals";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DEFAULT_SERVER_HOST: &str = "openshores.net";
const OLD_DEFAULT_SERVER_HOST: &str = "play.openshores.net";
const WINE_NAME: &str = "wine";
const DEFAULT_DARK_THEME: &str = "default-dark";
const DEFAULT_LIGHT_THEME: &str = "default-light";
const POST_LAUNCH_DO_NOTHING: &str = "do_nothing";
const POST_LAUNCH_MINIMIZE: &str = "minimize";
const POST_LAUNCH_EXIT: &str = "exit";
const POST_LAUNCH_OPEN_LOGS: &str = "open_logs";
const MAX_THEME_SIZE: usize = 1024 * 1024;
const LOG_FONT_SIZE_MIN: u8 = 8;
const LOG_FONT_SIZE_MAX: u8 = 24;
const DEFAULT_DARK_CSS: &str = include_str!("../../src/renderer/styles.css");

// OS-specific constants
#[cfg(windows)]
const ACCOUNT_REGISTRY_PATH: &str = r"Software\OpenShores\OpenShores\Account";
#[cfg(target_os = "linux")]
const ACCOUNT_REGISTRY_PATH: &str = r"S-1-5-21-0-0-0-1000\Software\OpenShores\OpenShores\Account";

type LauncherResult<T> = Result<T, String>;

#[derive(Clone, Default)]
struct AppState {
    active_operation: Arc<AtomicBool>,
    game_pid: Arc<Mutex<Option<u32>>>,
    designer_pid: Arc<Mutex<Option<u32>>>,
    pending_update: Arc<Mutex<Option<PendingUpdate>>>,
    changelog: Arc<Mutex<Option<ChangelogPayload>>>,
    server_statuses: Arc<Mutex<HashMap<String, ServerStatus>>>,
    account_clients: Arc<Mutex<HashMap<String, Client>>>,
}

#[derive(Clone)]
struct PendingUpdate {
    asset: GithubAsset,
    version: String,
}

struct OperationGuard(Arc<AtomicBool>);

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LauncherConfig {
    #[serde(rename = "installPath", skip_serializing_if = "Option::is_none")]
    install_path: Option<String>,
    #[serde(default)]
    servers: Option<Vec<ServerConfig>>,
    #[serde(
        rename = "connectedServerId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    connected_server_id: Option<String>,
    #[serde(default)]
    accounts: Vec<SavedAccount>,
    #[serde(rename = "developerMode", default)]
    developer_mode: bool,
    #[serde(
        rename = "selectedLogFile",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    selected_log_file: Option<String>,
    #[serde(
        rename = "logFontSize",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    log_font_size: Option<u8>,
    #[serde(rename = "selectedTheme", default = "default_theme")]
    selected_theme: String,
    #[serde(rename = "gameLaunchAction", default = "default_post_launch_action")]
    game_launch_action: String,
    #[serde(rename = "designerLaunchAction", default = "default_post_launch_action")]
    designer_launch_action: String,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            install_path: None,
            servers: Some(vec![default_server()]),
            connected_server_id: None,
            accounts: Vec::new(),
            developer_mode: false,
            selected_log_file: None,
            log_font_size: None,
            selected_theme: default_theme(),
            game_launch_action: default_post_launch_action(),
            designer_launch_action: default_post_launch_action(),
        }
    }
}

fn default_theme() -> String {
    DEFAULT_DARK_THEME.to_string()
}

fn default_post_launch_action() -> String {
    POST_LAUNCH_DO_NOTHING.to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerConfig {
    id: String,
    nickname: String,
    host: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedAccount {
    server_id: String,
    username: String,
    password_sha1: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

fn default_server() -> ServerConfig {
    ServerConfig {
        id: "openshores".to_string(),
        nickname: "OpenShores".to_string(),
        host: DEFAULT_SERVER_HOST.to_string(),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerStatus {
    login: String,
    scene: String,
    chat: String,
}

impl ServerStatus {
    fn unknown() -> Self {
        Self {
            login: "unknown".to_string(),
            scene: "unknown".to_string(),
            chat: "unknown".to_string(),
        }
    }

    fn online(&self) -> bool {
        self.login == "online" && self.scene == "online" && self.chat == "online"
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerSnapshot {
    id: String,
    nickname: String,
    host: String,
    status: ServerStatus,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LauncherSnapshot {
    launcher_version: String,
    installed: bool,
    install_path: String,
    manifest: Option<Value>,
    busy: bool,
    game_running: bool,
    designer_running: bool,
    installed_version: Option<String>,
    logged_in: bool,
    servers: Vec<ServerSnapshot>,
    connected_server_id: Option<String>,
    account_username: Option<String>,
    developer_mode: bool,
    log_font_size: Option<u8>,
    selected_theme: String,
    game_launch_action: String,
    designer_launch_action: String,
    themes: Vec<ThemeOption>,
    theme_css: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ThemeOption {
    id: String,
    name: String,
    built_in: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ThemeEditorDocument {
    id: Option<String>,
    name: String,
    css: String,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    phase: String,
    percent: u8,
    detail: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationStatusPayload {
    busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GameStatusPayload {
    process: String,
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdaterStatusPayload {
    state: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    release_url: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangelogPayload {
    title: String,
    changelog: String,
    release_url: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PathPayload {
    path: String
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientLogChunk {
    content: String,
    next_offset: u64,
    reset: bool,
    has_more: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogFileOption {
    path: String,
    name: String,
    selected: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    #[serde(default)]
    size: u64,
}

#[cfg(windows)]
fn local_app_data() -> LauncherResult<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "Windows Local AppData could not be located.".to_string())
}

#[cfg(target_os = "linux")]
fn local_app_data() -> LauncherResult<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|mut pb| { pb.push(".local/share"); pb })
        .ok_or_else(|| "Unix home directory could not be located".to_string())
}

fn launcher_data_path() -> LauncherResult<PathBuf> {
    Ok(local_app_data()?.join("OpenShores-Launcher"))
}

fn config_path() -> LauncherResult<PathBuf> {
    Ok(launcher_data_path()?.join("config.json"))
}

fn launcher_temp_path() -> LauncherResult<PathBuf> {
    Ok(launcher_data_path()?.join("temp"))
}

fn custom_themes_path() -> LauncherResult<PathBuf> {
    Ok(launcher_data_path()?.join("custom_themes"))
}

fn theme_file_name(id: &str) -> Option<&str> {
    id.strip_prefix("custom:")
        .filter(|name| is_safe_theme_file_name(name))
}

fn is_safe_theme_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.to_ascii_lowercase().ends_with(".css")
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, ' ' | '-' | '_' | '.'))
        && name != ".css"
        && !name.starts_with('.')
}

fn theme_file_name_from_name(name: &str) -> LauncherResult<String> {
    let trimmed = name.trim().trim_end_matches(|character: char| character == '.').trim();
    let stem = trimmed.strip_suffix(".css").unwrap_or(trimmed).trim();
    if stem.is_empty() || stem.len() > 120 {
        return Err("Theme name must contain between 1 and 120 characters.".to_string());
    }
    if !stem
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, ' ' | '-' | '_'))
    {
        return Err("Theme names may only use letters, numbers, spaces, hyphens, and underscores.".to_string());
    }
    Ok(format!("{stem}.css"))
}

fn theme_name_from_file(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name)
        .to_string()
}

fn validate_theme_css(css: &str) -> LauncherResult<()> {
    if css.len() > MAX_THEME_SIZE {
        return Err("Themes may not be larger than 1 MB.".to_string());
    }
    if css.contains('\0') {
        return Err("The theme contains invalid null characters.".to_string());
    }
    Ok(())
}

fn list_themes() -> LauncherResult<Vec<ThemeOption>> {
    let directory = custom_themes_path()?;
    fs::create_dir_all(&directory).map_err(error_string)?;
    let mut custom = Vec::new();
    for entry in fs::read_dir(&directory).map_err(error_string)? {
        let entry = entry.map_err(error_string)?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().map_err(error_string)?.is_file() && is_safe_theme_file_name(&file_name) {
            custom.push(ThemeOption {
                id: format!("custom:{file_name}"),
                name: theme_name_from_file(&file_name),
                built_in: false,
            });
        }
    }
    custom.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    let mut themes = vec![
        ThemeOption { id: DEFAULT_DARK_THEME.to_string(), name: "Default Dark [SYSTEM]".to_string(), built_in: true },
        ThemeOption { id: DEFAULT_LIGHT_THEME.to_string(), name: "Default Light [SYSTEM]".to_string(), built_in: true },
    ];
    themes.extend(custom);
    Ok(themes)
}

fn read_theme_css(theme_id: &str) -> LauncherResult<String> {
    match theme_id {
        DEFAULT_DARK_THEME => Ok(String::new()),
        DEFAULT_LIGHT_THEME => Ok(include_str!("../../src/renderer/default-light.css").to_string()),
        _ => {
            let file_name = theme_file_name(theme_id).ok_or_else(|| "Unknown theme.".to_string())?;
            let path = custom_themes_path()?.join(file_name);
            let metadata = fs::metadata(&path).map_err(|_| "The selected theme no longer exists.".to_string())?;
            if metadata.len() > MAX_THEME_SIZE as u64 {
                return Err("Themes may not be larger than 1 MB.".to_string());
            }
            fs::read_to_string(path).map_err(error_string)
        }
    }
}

fn default_theme_template() -> LauncherResult<String> {
    let start = DEFAULT_DARK_CSS
        .find(":root")
        .ok_or_else(|| "Default Dark does not contain a :root rule.".to_string())?;
    let open = DEFAULT_DARK_CSS[start..]
        .find('{')
        .map(|offset| start + offset)
        .ok_or_else(|| "Default Dark contains an invalid :root rule.".to_string())?;
    let bytes = DEFAULT_DARK_CSS.as_bytes();
    let mut depth = 0_u32;
    let mut quote = None;
    let mut escaped = false;
    let mut in_comment = false;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if in_comment {
            if byte == b'*' && next == Some(b'/') {
                in_comment = false;
                index += 2;
                continue;
            }
        } else if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == active_quote {
                quote = None;
            }
        } else if byte == b'/' && next == Some(b'*') {
            in_comment = true;
            index += 2;
            continue;
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Ok(format!("{}\n", DEFAULT_DARK_CSS[start..=index].trim()));
            }
        }
        index += 1;
    }
    Err("Default Dark contains an unterminated :root rule.".to_string())
}

fn default_install_path() -> LauncherResult<PathBuf> {
    Ok(local_app_data()?.join("OpenShores"))
}

fn legacy_install_path() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|path| path.join("Documents").join("OpenShores"))
}

fn program_files_install_path() -> PathBuf {
    env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join("OpenShores")
}


fn path_eq(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}


fn load_config() -> LauncherResult<LauncherConfig> {
    let path = config_path()?;
    let mut config = read_json::<LauncherConfig>(&path).unwrap_or_default();
    let mut changed = false;

    if config.install_path.is_none() {
        if let Some(roaming) = env::var_os("APPDATA").map(PathBuf::from) {
            for previous in [
                roaming.join("OpenShores Launcher").join("config.json"),
                roaming.join("openshores-launcher").join("config.json"),
            ] {
                if let Some(found) = read_json::<LauncherConfig>(&previous) {
                    if found.install_path.is_some() {
                        config = found;
                        changed = true;
                        break;
                    }
                }
            }
        }
    }

    if let Some(saved) = config.install_path.as_ref().map(PathBuf::from) {
        let is_legacy = legacy_install_path().is_some_and(|legacy| path_eq(&saved, &legacy))
            || path_eq(&saved, &program_files_install_path());
        if is_legacy {
            let manifest = read_json::<Value>(&saved.join(MANIFEST_FILE));
            if !is_managed_manifest(manifest.as_ref()) {
                config.install_path = Some(default_install_path()?.to_string_lossy().into_owned());
                changed = true;
            }
        }
    }

    if config.install_path.is_none() {
        config.install_path = Some(default_install_path()?.to_string_lossy().into_owned());
        changed = true;
    }
    if config.servers.is_none() {
        config.servers = Some(vec![default_server()]);
        changed = true;
    }
    if !matches!(
        config.game_launch_action.as_str(),
        POST_LAUNCH_DO_NOTHING | POST_LAUNCH_MINIMIZE | POST_LAUNCH_EXIT | POST_LAUNCH_OPEN_LOGS
    ) || (!config.developer_mode && config.game_launch_action == POST_LAUNCH_OPEN_LOGS)
    {
        config.game_launch_action = default_post_launch_action();
        changed = true;
    }
    if !matches!(
        config.designer_launch_action.as_str(),
        POST_LAUNCH_DO_NOTHING | POST_LAUNCH_MINIMIZE | POST_LAUNCH_EXIT
    ) {
        config.designer_launch_action = default_post_launch_action();
        changed = true;
    }
    if config
        .log_font_size
        .is_some_and(|size| !(LOG_FONT_SIZE_MIN..=LOG_FONT_SIZE_MAX).contains(&size))
    {
        config.log_font_size = None;
        changed = true;
    }
    for server in config.servers.iter_mut().flatten() {
        if server.host.eq_ignore_ascii_case(OLD_DEFAULT_SERVER_HOST) {
            server.host = DEFAULT_SERVER_HOST.to_string();
            changed = true;
        }
    }
    if config.servers.as_ref().is_none_or(|servers| servers.is_empty()) {
        config.servers = Some(vec![default_server()]);
        changed = true;
    }
    let server_ids: Vec<String> = config
        .servers
        .as_ref()
        .into_iter()
        .flatten()
        .map(|server| server.id.clone())
        .collect();
    let account_count = config.accounts.len();
    config.accounts.retain(|account| {
        server_ids.iter().any(|id| id == &account.server_id)
            && is_valid_password_sha1(&account.password_sha1)
    });
    if config.accounts.len() != account_count {
        changed = true;
    }
    if let Some(connected) = config.connected_server_id.as_deref() {
        let exists = config
            .servers
            .as_ref()
            .is_some_and(|servers| servers.iter().any(|server| server.id == connected));
        if !exists {
            config.connected_server_id = None;
            changed = true;
        }
    }
    if config.connected_server_id.is_none() {
        config.connected_server_id = server_ids.first().cloned();
        changed = config.connected_server_id.is_some() || changed;
    }
    if changed || !path.is_file() {
        write_json(&path, &config)?;
    }
    Ok(config)
}

fn save_install_path(install_path: &Path) -> LauncherResult<()> {
    let mut config = load_config()?;
    config.install_path = Some(install_path.to_string_lossy().into_owned());
    write_json(&config_path()?, &config)
}

fn save_developer_mode(enabled: bool) -> LauncherResult<()> {
    let mut config = load_config()?;
    config.developer_mode = enabled;
    if !enabled && config.game_launch_action == POST_LAUNCH_OPEN_LOGS {
        config.game_launch_action = default_post_launch_action();
    }
    write_json(&config_path()?, &config)
}

fn save_selected_log_file(path: Option<String>) -> LauncherResult<()> {
    let mut config = load_config()?;
    config.selected_log_file = path;
    write_json(&config_path()?, &config)
}

fn save_log_font_size(size: u8) -> LauncherResult<()> {
    if !(LOG_FONT_SIZE_MIN..=LOG_FONT_SIZE_MAX).contains(&size) {
        return Err(format!(
            "Log font size must be between {LOG_FONT_SIZE_MIN} and {LOG_FONT_SIZE_MAX}."
        ));
    }
    let mut config = load_config()?;
    config.log_font_size = Some(size);
    write_json(&config_path()?, &config)
}

fn save_post_launch_action(process: &str, action: String) -> LauncherResult<()> {
    let mut config = load_config()?;
    let common_action = matches!(
        action.as_str(),
        POST_LAUNCH_DO_NOTHING | POST_LAUNCH_MINIMIZE | POST_LAUNCH_EXIT
    );
    match process {
        "game" if common_action || (action == POST_LAUNCH_OPEN_LOGS && config.developer_mode) => {
            config.game_launch_action = action;
        }
        "designer" if common_action => config.designer_launch_action = action,
        "game" => return Err("Open Logs requires Developer mode.".to_string()),
        "designer" => return Err("Unknown Designer launch action.".to_string()),
        _ => return Err("Unknown launch process.".to_string()),
    }
    write_json(&config_path()?, &config)
}

fn save_selected_theme(selected_theme: String) -> LauncherResult<()> {
    let mut config = load_config()?;
    config.selected_theme = selected_theme;
    write_json(&config_path()?, &config)
}

fn get_snapshot(state: &AppState) -> LauncherResult<LauncherSnapshot> {
    let mut config = load_config()?;
    let themes = list_themes()?;
    if !themes.iter().any(|theme| theme.id == config.selected_theme) {
        config.selected_theme = default_theme();
        write_json(&config_path()?, &config)?;
    }
    let theme_css = read_theme_css(&config.selected_theme)?;
    let install_path = PathBuf::from(
        config
            .install_path
            .clone()
            .unwrap_or(default_install_path()?.to_string_lossy().into_owned()),
    );
    let manifest = read_json::<Value>(&install_path.join(MANIFEST_FILE));
    let installed = manifest.is_some() && install_path.join(GAME_EXE).is_file();
    let game_running = state.game_pid.lock().map_err(error_string)?.is_some();
    let designer_running = state.designer_pid.lock().map_err(error_string)?.is_some();
    let statuses = state.server_statuses.lock().map_err(error_string)?;
    let servers = config
        .servers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|server| ServerSnapshot {
            status: statuses
                .get(&server.id)
                .cloned()
                .unwrap_or_else(ServerStatus::unknown),
            id: server.id,
            nickname: server.nickname,
            host: server.host,
        })
        .collect();
    let account = config.connected_server_id.as_deref().and_then(|server_id| {
        config
            .accounts
            .iter()
            .find(|account| account.server_id == server_id)
    });
    let account_username = account.map(|account| account.username.clone());
    let logged_in = account.is_some_and(|account| account.session.is_some());
    let installed_version = manifest
        .as_ref()
        .filter(|_| installed)
        .and_then(|manifest| manifest.get("version"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(LauncherSnapshot {
        launcher_version: env!("OPENSHORES_LAUNCHER_VERSION").to_string(),
        installed,
        install_path: install_path.to_string_lossy().into_owned(),
        manifest,
        busy: state.active_operation.load(Ordering::SeqCst),
        game_running,
        designer_running,
        installed_version,
        logged_in,
        servers,
        connected_server_id: config.connected_server_id,
        account_username,
        developer_mode: config.developer_mode,
        log_font_size: config.log_font_size,
        selected_theme: config.selected_theme,
        game_launch_action: config.game_launch_action,
        designer_launch_action: config.designer_launch_action,
        themes,
        theme_css,
    })
}

fn any_game_process_running(state: &AppState) -> LauncherResult<bool> {
    Ok(state.game_pid.lock().map_err(error_string)?.is_some()
        || state.designer_pid.lock().map_err(error_string)?.is_some())
}

fn begin_operation(state: &AppState) -> LauncherResult<OperationGuard> {
    state
        .active_operation
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| "Another launcher operation is already running.".to_string())?;
    Ok(OperationGuard(state.active_operation.clone()))
}

fn emit_progress(app: &AppHandle, phase: &str, percent: f64, detail: impl Into<String>) {
    let _ = app.emit(
        "operation-progress",
        ProgressPayload {
            phase: phase.to_string(),
            percent: percent.round().clamp(0.0, 100.0) as u8,
            detail: detail.into(),
        },
    );
}

fn emit_operation_status(app: &AppHandle, busy: bool, error: Option<String>) {
    let _ = app.emit("operation-status", OperationStatusPayload { busy, error });
}

fn updater_status(state: &str, message: impl Into<String>) -> UpdaterStatusPayload {
    UpdaterStatusPayload {
        state: state.to_string(),
        message: message.into(),
        progress: None,
        changelog_title: None,
        changelog: None,
        release_url: None,
    }
}

fn emit_updater(app: &AppHandle, state: &str, message: impl Into<String>, progress: Option<u8>) {
    let mut status = updater_status(state, message);
    status.progress = progress;
    let _ = app.emit(
        "updater-status",
        status,
    );
}

fn http_client() -> LauncherResult<Client> {
    Client::builder()
        .user_agent(format!(
            "OpenShores-Launcher/{}",
            env!("OPENSHORES_LAUNCHER_VERSION")
        ))
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .map_err(error_string)
}

fn account_client() -> LauncherResult<Client> {
    Client::builder()
        .user_agent(format!(
            "OpenShores-Launcher/{}",
            env!("OPENSHORES_LAUNCHER_VERSION")
        ))
        .cookie_store(true)
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(error_string)
}

fn normalize_server_host(value: &str) -> LauncherResult<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Enter a server hostname.".to_string());
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let parsed =
        Url::parse(&candidate).map_err(|_| "Enter a valid server hostname.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Server addresses must use HTTP or HTTPS.".to_string());
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("Server addresses cannot include a path, query, or fragment.".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "Enter a valid server hostname.".to_string())?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() || host.contains(' ') {
        return Err("Enter a valid server hostname.".to_string());
    }
    Ok(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn account_domain(host: &str) -> String {
    let hostname = host.split(':').next().unwrap_or(host).trim_end_matches('.');
    if hostname.parse::<std::net::IpAddr>().is_ok() || hostname.eq_ignore_ascii_case("localhost") {
        return host.to_string();
    }
    List.domain(hostname.as_bytes())
        .and_then(|domain| std::str::from_utf8(domain.as_bytes()).ok())
        .unwrap_or(hostname)
        .to_string()
}

fn server_api_url(host: &str, endpoint: &str) -> LauncherResult<Url> {
    Url::parse(&format!("https://{}/api/{endpoint}", account_domain(host))).map_err(error_string)
}

fn password_sha1_hex(password: &str) -> String {
    Sha1::digest(password.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn is_valid_password_sha1(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hazeron_password_value_from_hex(password_sha1: &str) -> LauncherResult<String> {
    if !is_valid_password_sha1(password_sha1) {
        return Err("The saved account password hash is invalid.".to_string());
    }
    let payload = (0..40)
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&password_sha1[index..index + 2], 16)
                .map(char::from)
                .map_err(|_| "The saved account password hash is invalid.".to_string())
        })
        .collect::<LauncherResult<String>>()?;

    Ok(format!("@ByteArray({payload})"))
}

fn wine_password_value_from_hex(password_sha1: &str) -> LauncherResult<String> {
    if !is_valid_password_sha1(password_sha1) {
        return Err("The saved account password hash is invalid.".to_string());
    }

    let start_bytes: Vec<u8> = "@ByteArray(".bytes().collect();
    let sha_bytes: Vec<u8> = (0..40)
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&password_sha1[index..index + 2], 16)
                .map_err(|_| "The saved account password hash is invalid.".to_string())
        })
        .collect::<LauncherResult<Vec<_>>>()?;
    let end_bytes: Vec<u8> = ")".bytes().collect();

    let bytes: Vec<u8>= start_bytes.into_iter()
        .chain(sha_bytes)
        .chain(end_bytes)
        .collect();

    let hex_bytes: Vec<String> = bytes
        .iter()
        .flat_map(|byte| [format!("{:02x}", byte), "00".to_string()])
        .collect();

    let hex_str = hex_bytes.join(",");

    Ok(hex_str)
}

fn wine_password_registry_import(key: &str, password_value: &str) -> Vec<u8> {
    let text = format!(
        "Windows Registry Editor Version 5.00\r\n\r\n[{key}]\r\n\"Password\"=hex:{}\r\n",
        password_value
    );
    let mut bytes = vec![0xff, 0xfe];
    for code_unit in text.encode_utf16() {
        bytes.extend_from_slice(&code_unit.to_le_bytes());
    }

    bytes
}

fn response_error(response: Response, fallback: &str) -> String {
    let status = response.status();
    response
        .json::<Value>()
        .ok()
        .and_then(|value| {
            value
                .get("detail")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("{fallback} ({status})."))
}

fn server_by_id(config: &LauncherConfig, id: &str) -> LauncherResult<ServerConfig> {
    config
        .servers
        .as_ref()
        .and_then(|servers| servers.iter().find(|server| server.id == id))
        .cloned()
        .ok_or_else(|| "That server no longer exists.".to_string())
}

fn connected_server(config: &LauncherConfig) -> LauncherResult<ServerConfig> {
    let id = config
        .connected_server_id
        .as_deref()
        .ok_or_else(|| "Connect to a server first.".to_string())?;
    server_by_id(config, id)
}

fn fetch_server_status(client: &Client, server: &ServerConfig) -> ServerStatus {
    let Ok(url) = server_api_url(&server.host, "status") else {
        return ServerStatus::unknown();
    };
    client
        .get(url)
        .header("Cache-Control", "no-store")
        .send()
        .ok()
        .filter(|response| response.status().is_success())
        .and_then(|response| response.json::<ServerStatus>().ok())
        .unwrap_or_else(ServerStatus::unknown)
}

#[cfg(windows)]
fn write_game_account_registry(account: Option<&SavedAccount>) -> LauncherResult<bool> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = current_user
        .create_subkey(ACCOUNT_REGISTRY_PATH)
        .map_err(error_string)?;
    if let Some(account) = account {
        let password = hazeron_password_value_from_hex(&account.password_sha1)?;
        key.set_value("Name", &account.username)
            .map_err(error_string)?;
        key.set_value("Password", &password).map_err(error_string)?;
        key.set_value("SavePass", &1_u32).map_err(error_string)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
fn run_wine_registry_command(wine: &Path, arguments: &[&str]) -> LauncherResult<()> {
    let status = Command::new(wine)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .args(arguments)
        .status()
        .map_err(|error| format!("wine registry command failed: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("wine registry command exited with {status}."))
    }
}

#[cfg(target_os = "linux")]
fn write_wine_password_registry(
    wine: &Path,
    key: &str,
    password_value: &str,
) -> LauncherResult<()> {
    let temp = launcher_temp_path()?;
    fs::create_dir_all(&temp).map_err(error_string)?;
    let import_path = temp.join(format!("account-password-{}.reg", install::unique_token()));
    fs::write(
        &import_path,
        wine_password_registry_import(key, password_value),
    )
    .map_err(error_string)?;
    let import_path_text = import_path.to_string_lossy().into_owned();
    let result = run_wine_registry_command(wine, &["REG", "IMPORT", &import_path_text]);
    let _ = fs::remove_file(import_path);
    result
}

#[cfg(target_os = "linux")]
fn write_game_account_registry(account: Option<&SavedAccount>) -> LauncherResult<bool> {
    let key = format!("HKEY_USERS\\{}", ACCOUNT_REGISTRY_PATH);
    let wine = which(WINE_NAME).map_err(error_string)?;

    if let Some(account) = account {
        run_wine_registry_command(
            &wine,
            &[
                "REG",
                "ADD",
                &key,
                "/v",
                "Name",
                "/t",
                "REG_SZ",
                "/d",
                &account.username,
                "/f",
            ],
        )?;
        let password = wine_password_value_from_hex(&account.password_sha1)?;
        write_wine_password_registry(&wine, &key, &password)?;
        run_wine_registry_command(
            &wine,
            &["REG", "ADD", &key, "/v", "SavePass", "/t", "REG_DWORD", "/d", "1", "/f"],
        )?;
        Ok(true)
    } else {
        Ok(false)
    }

}


fn checked_response(response: Response, description: &str) -> LauncherResult<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(format!("{description} failed ({}).", response.status()))
    }
}


fn verify_asset_digest(path: &Path, asset: &GithubAsset) -> LauncherResult<()> {
    if let Some(expected) = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
    {
        let actual = chain::sha256_file(path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("Checksum verification failed for {}.", asset.name));
        }
    }
    Ok(())
}

fn verify_launcher_digest(path: &Path, asset: &GithubAsset) -> LauncherResult<()> {
    if asset.digest.is_none() {
        return Err("The website did not provide a checksum for the launcher update.".to_string());
    }
    verify_asset_digest(path, asset)
}



fn is_session_value(value: &str) -> bool {
    (16..=128).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}


fn site_client(account: &SavedAccount) -> LauncherResult<Client> {
    let session = account
        .session
        .as_deref()
        .filter(|value| is_session_value(value))
        .ok_or_else(|| LOGIN_NEEDED.to_string())?;
    let mut cookie = reqwest::header::HeaderValue::from_str(&format!("{SESSION_COOKIE}={session}"))
        .map_err(error_string)?;
    cookie.set_sensitive(true);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::COOKIE, cookie);
    Client::builder()
        .user_agent(format!(
            "OpenShores-Launcher/{}",
            env!("OPENSHORES_LAUNCHER_VERSION")
        ))
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .default_headers(headers)
        .build()
        .map_err(error_string)
}

fn fetch_chain(config: &LauncherConfig) -> LauncherResult<Site> {
    let server = connected_server(config)?;
    let account = config
        .accounts
        .iter()
        .find(|account| account.server_id == server.id)
        .ok_or_else(|| LOGIN_NEEDED.to_string())?;
    let client = site_client(account)?;
    let response = client
        .get(server_api_url(&server.host, "patches")?)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .map_err(|_| "Could not reach the website.".to_string())?;
    match response.status().as_u16() {
        401 | 403 => return Err(LOGIN_EXPIRED.to_string()),
        404 => return Err("There are no downloads right now.".to_string()),
        _ => {}
    }
    let chain: chain::Chain = checked_response(response, "Patch list download")?
        .json()
        .map_err(|_| "The website's patch list could not be read.".to_string())?;
    chain.validate()?;
    Ok(Site {
        client,
        origin: format!("https://{}", account_domain(&server.host)),
        chain,
    })
}

fn downloads_path() -> LauncherResult<PathBuf> {
    Ok(launcher_temp_path()?.join("downloads"))
}


fn ensure_hpatchz() -> LauncherResult<PathBuf> {
    install::ensure_hpatchz(&launcher_data_path()?)
}


fn install_game_sync(
    app: &AppHandle,
    state: &AppState,
    install_path: PathBuf,
) -> LauncherResult<LauncherSnapshot> {
    let _guard = begin_operation(state)?;
    if any_game_process_running(state)? {
        return Err("Close OpenShores before updating it.".to_string());
    }
    let config = load_config()?;
    emit_progress(
        app,
        "Checking the game version",
        1.0,
        "Asking the website for the patch list...",
    );
    let access = fetch_chain(&config)?;
    let report = |phase: &str, percent: f64, detail: String| emit_progress(app, phase, percent, detail);
    install::install(&report, &access, &ensure_hpatchz, &downloads_path()?, install_path.clone())?;
    save_install_path(&install_path)?;
    get_snapshot(state)
}

fn check_game_update_sync() -> LauncherResult<UpdaterStatusPayload> {
    let config = load_config()?;
    let install_path = PathBuf::from(config.install_path.clone().unwrap());
    if !install_path.join(GAME_EXE).is_file() {
        return Ok(updater_status("current", "Install OpenShores before checking for game updates."));
    }
    let access = match fetch_chain(&config) {
        Ok(access) => access,
        Err(error) if error == LOGIN_NEEDED || error == LOGIN_EXPIRED => {
            return Ok(updater_status("login", error));
        }
        Err(error) => return Err(error),
    };
    let chain = &access.chain;
    let chain::Installed::Version(installed) = chain.identify(&install_path)? else {
        return Ok(updater_status(
            "current",
            "The game folder does not hold a known version of OpenShores.",
        ));
    };
    let Some(patches) = chain.route(&installed) else {
        return Ok(updater_status(
            "current",
            format!("No patches lead from version {installed} to version {}.", chain.latest),
        ));
    };
    if patches.is_empty() {
        return Ok(updater_status("current", format!("OpenShores {installed} is up to date.")));
    }
    let notes = patches
        .iter()
        .rev()
        .filter_map(|patch| chain.version(&patch.to))
        .map(|version| {
            let title = if version.title.is_empty() { &version.id } else { &version.title };
            if version.notes.trim().is_empty() {
                format!("## {title}")
            } else {
                format!("## {title}\n\n{}", version.notes.trim())
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let mut status = updater_status(
        "available",
        format!("OpenShores {} is available (installed: {installed}).", chain.latest),
    );
    status.changelog_title = Some(format!("OpenShores {}", chain.latest));
    status.changelog = Some(notes);
    Ok(status)
}

fn uninstall_game_sync(app: &AppHandle, state: &AppState) -> LauncherResult<LauncherSnapshot> {
    if any_game_process_running(state)? {
        return Err("Close OpenShores before uninstalling it.".to_string());
    }
    let config = load_config()?;
    let install_path = PathBuf::from(config.install_path.unwrap());
    let manifest = read_json::<Value>(&install_path.join(MANIFEST_FILE));
    if !is_managed_manifest(manifest.as_ref()) {
        return Err(
            "This folder is not a launcher-managed installation, so it was left untouched."
                .to_string(),
        );
    }
    let confirmed = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("Uninstall OpenShores")
        .set_description(format!(
            "Remove all launcher-managed game files in {}?",
            install_path.display()
        ))
        .set_buttons(MessageButtons::YesNo)
        .show();
    if confirmed != MessageDialogResult::Yes {
        return get_snapshot(state);
    }
    let _guard = begin_operation(state)?;
    emit_progress(
        app,
        "Uninstalling OpenShores",
        25.0,
        "Removing launcher-managed game files...",
    );
    fs::remove_dir_all(&install_path).map_err(error_string)?;
    emit_progress(
        app,
        "OpenShores uninstalled",
        100.0,
        "Launcher settings were preserved.",
    );
    get_snapshot(state)
}

fn main_game_launch_arguments(developer_mode: bool) -> Vec<&'static str> {
    let mut arguments = Vec::new();
    if developer_mode {
        arguments.push("-log");
    }
    arguments
}

fn launch_game_sync(
    app: &AppHandle,
    state: &AppState,
    designer_mode: Option<&str>,
) -> LauncherResult<LauncherSnapshot> {
    let config = load_config()?;
    let install_path = PathBuf::from(config.install_path.clone().unwrap());
    let executable = install_path.join(GAME_EXE);

    if !executable.is_file() {
        return Err("OpenShores is not installed.".to_string());
    }
    let process_name = if designer_mode.is_some() {
        "designer"
    } else {
        "game"
    };
    let process_pid = if designer_mode.is_some() {
        state.designer_pid.clone()
    } else {
        state.game_pid.clone()
    };
    {
        let mut pid = process_pid.lock().map_err(error_string)?;
        if pid.is_some() {
            return get_snapshot(state);
        }

        // SoH exe needs to be run through Wine on Linux
        let mut command = if cfg!(target_os = "linux") {
            let wine = which(WINE_NAME).map_err(error_string)?;

            let mut wine_command = Command::new(&wine);
            wine_command.arg(&executable);
            wine_command
        } else {
            Command::new(&executable)
        };

        if let Some(mode) = designer_mode {
            command.arg(designer_launch_argument(mode)?);
        } else {
            for argument in main_game_launch_arguments(config.developer_mode) {
                command.arg(argument);
            }
            if let Some(server_id) = config.connected_server_id.as_deref() {
                let server = server_by_id(&config, server_id)?;
                let active_account = config
                    .accounts
                    .iter()
                    .find(|account| account.server_id == server.id);
                write_game_account_registry(active_account)?;
            }
        }
        let mut child = command
            .current_dir(&install_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(error_string)?;
        let child_pid = child.id();
        *pid = Some(child_pid);
        let tracked_pid = process_pid.clone();
        let tracked_process = process_name.to_string();
        let app_handle = app.clone();
        thread::spawn(move || {
            let result = child.wait();
            if let Ok(mut current) = tracked_pid.lock() {
                if *current == Some(child_pid) {
                    *current = None;
                }
            }
            let error = result.err().map(|value| value.to_string());
            let _ = app_handle.emit(
                "game-status",
                GameStatusPayload {
                    process: tracked_process,
                    running: false,
                    error,
                },
            );
        });
    }
    let _ = app.emit(
        "game-status",
        GameStatusPayload {
            process: process_name.to_string(),
            running: true,
            error: None,
        },
    );
    get_snapshot(state)
}

fn download_launcher_update(
    app: &AppHandle,
    client: &Client,
    asset: &GithubAsset,
    version: &str,
) -> LauncherResult<PathBuf> {
    let temp = launcher_temp_path()?;
    fs::create_dir_all(&temp).map_err(error_string)?;
    let destination = temp.join(format!("OpenShores-Launcher-{version}.download.exe"));
    emit_updater(app, "downloading", format!("Downloading launcher {version}"), Some(0));
    let result = (|| {
        let mut response = checked_response(
            client
                .get(&asset.browser_download_url)
                .send()
                .map_err(error_string)?,
            "Launcher download",
        )?;
        let total = response.content_length().unwrap_or(asset.size);
        let mut output = File::create(&destination).map_err(error_string)?;
        let mut buffer = vec![0_u8; 128 * 1024];
        let mut received = 0_u64;
        loop {
            let count = response.read(&mut buffer).map_err(error_string)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count]).map_err(error_string)?;
            received += count as u64;
            let percent = if total > 0 {
                (received.saturating_mul(100) / total).min(100)
            } else {
                0
            };
            emit_updater(app, "downloading", format!("Downloading launcher {version}"), Some(percent as u8));
        }
        output.flush().map_err(error_string)?;
        verify_launcher_digest(&destination, asset)?;
        Ok(destination.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&destination);
    }
    result
}

#[derive(Clone, Debug, Deserialize)]
struct LauncherOffer {
    version: String,
    size: u64,
    sha256: String,
    url: String,
    #[serde(default)]
    notes: String,
}

impl LauncherOffer {
    fn usable(&self) -> bool {
        !self.version.is_empty()
            && self.version.len() <= 32
            && self.version.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            && self.url.starts_with("/api/")
            && !self.url.contains("..")
            && !self.url.contains(['\\', '#'])
    }

    fn newer_than(&self, current: &str) -> bool {
        match (semver::Version::parse(&self.version), semver::Version::parse(current)) {
            (Ok(offered), Ok(running)) => offered > running,
            _ => !self.version.eq_ignore_ascii_case(current),
        }
    }
}

fn check_updates_sync(
    app: &AppHandle,
    state: &AppState,
    manual: bool,
) -> LauncherResult<UpdaterStatusPayload> {
    if cfg!(target_os = "linux") {
        return check_appimage_update_sync(app, state, manual);
    }
    let config = load_config()?;
    let server = connected_server(&config)?;
    let client = http_client()?;
    let response = client
        .get(server_api_url(&server.host, "launcher")?)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .map_err(|_| "Could not reach the website.".to_string())?;
    if response.status().as_u16() == 404 {
        *state.pending_update.lock().map_err(error_string)? = None;
        return Ok(updater_status("current", "The website offers no launcher update."));
    }
    let offer: LauncherOffer = checked_response(response, "Launcher update check")?
        .json()
        .map_err(|_| "The website's launcher information could not be read.".to_string())?;
    if !offer.usable() {
        return Err("The website's launcher information could not be read.".to_string());
    }
    let current_version = env!("OPENSHORES_LAUNCHER_VERSION");
    if !offer.newer_than(current_version) {
        *state.pending_update.lock().map_err(error_string)? = None;
        return Ok(updater_status("current", "Launcher is up to date."));
    }
    let site = format!("https://{}", account_domain(&server.host));
    *state.pending_update.lock().map_err(error_string)? = Some(PendingUpdate {
        asset: GithubAsset {
            name: LAUNCHER_FILE.to_string(),
            browser_download_url: format!("{site}{}", offer.url),
            digest: Some(format!("sha256:{}", offer.sha256)),
            size: offer.size,
        },
        version: offer.version.clone(),
    });
    let mut status = updater_status("available", format!("Launcher {} is available.", offer.version));
    if !offer.notes.trim().is_empty() {
        status.changelog_title = Some(format!("Launcher {}", offer.version));
        status.changelog = Some(offer.notes.trim().to_string());
        status.release_url = Some(format!("{site}/download"));
    }
    if !manual {
        let _ = app.emit("updater-status", status.clone());
    }
    Ok(status)
}

const LAUNCHER_RELEASES_API: &str = "https://api.github.com/repos/Norway174/OpenShores-Launcher/releases?per_page=30";
const LAUNCHER_APPIMAGE: &str = "OpenShores-Launcher-amd64.AppImage";

#[derive(Clone, Debug, Deserialize)]
struct LauncherRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

fn check_appimage_update_sync(
    app: &AppHandle,
    state: &AppState,
    manual: bool,
) -> LauncherResult<UpdaterStatusPayload> {
    if env::var_os("APPIMAGE").is_none() {
        *state.pending_update.lock().map_err(error_string)? = None;
        return Ok(updater_status("current", "This launcher is updated by the package manager that installed it."));
    }
    let client = http_client()?;
    let response = client
        .get(LAUNCHER_RELEASES_API)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .map_err(|_| "Could not reach GitHub.".to_string())?;
    if response.status().as_u16() == 404 {
        *state.pending_update.lock().map_err(error_string)? = None;
        return Ok(updater_status("current", "No launcher releases are published."));
    }
    let releases: Vec<LauncherRelease> = checked_response(response, "Launcher update check")?
        .json()
        .map_err(|_| "The launcher's releases could not be read.".to_string())?;
    let current_version = env!("OPENSHORES_LAUNCHER_VERSION");
    let newest = releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            let version = semver::Version::parse(release.tag_name.trim().trim_start_matches(['v', 'V'])).ok()?;
            let asset = release.assets.iter().find(|asset| asset.name.eq_ignore_ascii_case(LAUNCHER_APPIMAGE))?;
            Some((version, release, asset))
        })
        .max_by(|left, right| left.0.cmp(&right.0));
    let newer = match (&newest, semver::Version::parse(current_version)) {
        (Some((offered, _, _)), Ok(running)) => *offered > running,
        _ => false,
    };
    let Some((version, release, asset)) = newest.filter(|_| newer) else {
        *state.pending_update.lock().map_err(error_string)? = None;
        return Ok(updater_status("current", "Launcher is up to date."));
    };
    let version = version.to_string();
    *state.pending_update.lock().map_err(error_string)? = Some(PendingUpdate {
        asset: asset.clone(),
        version: version.clone(),
    });
    let mut status = updater_status("available", format!("Launcher {version} is available."));
    if let Some(notes) = release.body.as_deref().map(str::trim).filter(|notes| !notes.is_empty()) {
        status.changelog_title = Some(format!("Launcher {version}"));
        status.changelog = Some(notes.to_string());
        status.release_url = Some(release.html_url.clone());
    }
    if !manual {
        let _ = app.emit("updater-status", status.clone());
    }
    Ok(status)
}

fn report_previous_update_error(app: &AppHandle) {
    let Ok(path) = launcher_data_path().map(|path| path.join("update-error.log")) else {
        return;
    };
    if let Ok(message) = fs::read_to_string(&path) {
        let _ = fs::remove_file(path);
        if !message.trim().is_empty() {
            emit_updater(app, "error", message.trim(), None);
        }
    }
}

fn cleanup_legacy_electron_data() -> LauncherResult<bool> {
    let root = launcher_data_path()?;
    let legacy_runtime = root.join("runtime").join("electron-43.3.0-win32-x64");
    let legacy_app = root.join("app");
    let runtime_marker = legacy_runtime.join(".openshores-runtime");
    let app_archive = legacy_app.join("app.asar");
    let legacy_detected = runtime_marker.is_file() || app_archive.is_file();
    if !legacy_detected {
        return Ok(false);
    }

    if runtime_marker.is_file() {
        fs::remove_dir_all(&legacy_runtime).map_err(error_string)?;
        let runtime_parent = root.join("runtime");
        if runtime_parent
            .read_dir()
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
        {
            let _ = fs::remove_dir(runtime_parent);
        }
    }

    for file in [app_archive, legacy_app.join("resources").join("xdelta3-3.0.11-x86_64.exe")] {
        if file.is_file() {
            fs::remove_file(file).map_err(error_string)?;
        }
    }
    let resources = legacy_app.join("resources");
    if resources
        .read_dir()
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
    {
        let _ = fs::remove_dir(resources);
    }
    if legacy_app
        .read_dir()
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
    {
        let _ = fs::remove_dir(legacy_app);
    }

    for directory in [
        "blob_storage",
        "Cache",
        "Code Cache",
        "DawnGraphiteCache",
        "DawnWebGPUCache",
        "GPUCache",
        "Local Storage",
        "Network",
        "Session Storage",
        "Shared Dictionary",
    ] {
        let path = root.join(directory);
        if path.is_dir() {
            let _ = fs::remove_dir_all(path);
        }
    }
    for file in [
        "DIPS",
        "DIPS-wal",
        "Local State",
        "lockfile",
        "Preferences",
        "SharedStorage",
        "SharedStorage-wal",
    ] {
        let path = root.join(file);
        if path.is_file() {
            let _ = fs::remove_file(path);
        }
    }
    Ok(true)
}

fn batch_escape(value: &Path) -> String {
    value.to_string_lossy().replace('%', "%%").replace('"', "")
}


fn copy_installation_tree(
    app: &AppHandle,
    source: &Path,
    destination: &Path,
    total_bytes: u64,
    copied_bytes: &mut u64,
) -> LauncherResult<()> {
    fs::create_dir_all(destination).map_err(error_string)?;
    for entry in fs::read_dir(source).map_err(error_string)? {
        let entry = entry.map_err(error_string)?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().map_err(error_string)?;
        if file_type.is_symlink() {
            return Err(
                "The installation contains a symbolic link and cannot be moved safely.".to_string(),
            );
        }
        if file_type.is_dir() {
            copy_installation_tree(
                app,
                &source_path,
                &destination_path,
                total_bytes,
                copied_bytes,
            )?;
        } else if file_type.is_file() {
            let bytes = fs::copy(&source_path, &destination_path).map_err(error_string)?;
            *copied_bytes = copied_bytes.saturating_add(bytes);
            let percent = if total_bytes == 0 {
                90.0
            } else {
                8.0 + (*copied_bytes as f64 / total_bytes as f64) * 82.0
            };
            emit_progress(
                app,
                "Moving OpenShores",
                percent,
                format!("Copying {}", entry.file_name().to_string_lossy()),
            );
        }
    }
    Ok(())
}

fn move_installation_sync(
    app: &AppHandle,
    state: &AppState,
    source: PathBuf,
    destination: PathBuf,
) -> LauncherResult<LauncherSnapshot> {
    if any_game_process_running(state)? {
        return Err("Close OpenShores before moving the installation.".to_string());
    }
    let source = source.canonicalize().map_err(error_string)?;
    let destination = destination.canonicalize().map_err(error_string)?;
    if path_eq(&source, &destination) {
        return get_snapshot(state);
    }
    if destination.starts_with(&source) {
        return Err("Choose a folder outside the current installation.".to_string());
    }
    if fs::read_dir(&destination)
        .map_err(error_string)?
        .next()
        .is_some()
    {
        return Err("Choose an empty destination folder.".to_string());
    }
    if !is_managed_manifest(read_json::<Value>(&source.join(MANIFEST_FILE)).as_ref()) {
        return Err("The current installation is not managed by this launcher.".to_string());
    }

    let _guard = begin_operation(state)?;
    emit_progress(
        app,
        "Moving OpenShores",
        4.0,
        format!("Preparing {}", destination.display()),
    );

    fs::remove_dir(&destination).map_err(error_string)?;
    if fs::rename(&source, &destination).is_ok() {
        save_install_path(&destination)?;
        emit_progress(
            app,
            "OpenShores moved",
            100.0,
            format!("Installation moved to {}", destination.display()),
        );
        return get_snapshot(state);
    }

    fs::create_dir_all(&destination).map_err(error_string)?;
    let total_bytes = directory_file_bytes(&source)?;
    let mut copied_bytes = 0u64;
    copy_installation_tree(app, &source, &destination, total_bytes, &mut copied_bytes)?;
    if !destination.join(GAME_EXE).is_file()
        || !is_managed_manifest(read_json::<Value>(&destination.join(MANIFEST_FILE)).as_ref())
    {
        return Err(
            "The copied installation could not be verified; the original was preserved."
                .to_string(),
        );
    }
    emit_progress(
        app,
        "Moving OpenShores",
        94.0,
        "Removing the original installation...",
    );
    save_install_path(&destination)?;
    fs::remove_dir_all(&source).map_err(error_string)?;
    emit_progress(
        app,
        "OpenShores moved",
        100.0,
        format!("Installation moved to {}", destination.display()),
    );
    get_snapshot(state)
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> LauncherResult<LauncherSnapshot> {
    get_snapshot(state.inner())
}

fn is_log_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("txt")
        })
}

fn normalized_relative_log_path(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !is_log_file(path)
    {
        return None;
    }
    Some(path.to_string_lossy().replace('\\', "/"))
}

fn ignored_log_directory(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name.eq_ignore_ascii_case(PATCH_ORIGINALS_DIR)
            || name.eq_ignore_ascii_case("ExampleStory")
    })
}

fn collect_log_files(root: &Path, directory: &Path, files: &mut Vec<String>) -> LauncherResult<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return Ok(()),
        Err(error) => return Err(error_string(error)),
    };
    for entry in entries {
        let entry = entry.map_err(error_string)?;
        let file_type = entry.file_type().map_err(error_string)?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if !ignored_log_directory(&path) {
                collect_log_files(root, &path, files)?;
            }
        } else if file_type.is_file() && is_log_file(&path) {
            if let Ok(relative) = path.strip_prefix(root) {
                if let Some(relative) = normalized_relative_log_path(relative) {
                    files.push(relative);
                }
            }
        }
    }
    Ok(())
}

fn discover_log_files(config: &LauncherConfig) -> LauncherResult<Vec<String>> {
    let root = PathBuf::from(
        config
            .install_path
            .clone()
            .unwrap_or(default_install_path()?.to_string_lossy().into_owned()),
    );
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_log_files(&root, &root, &mut files)?;
    files.sort_by(|left, right| {
        let left_client = Path::new(left)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(CLIENT_LOG));
        let right_client = Path::new(right)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(CLIENT_LOG));
        right_client
            .cmp(&left_client)
            .then_with(|| left.to_lowercase().cmp(&right.to_lowercase()))
    });
    Ok(files)
}

fn selected_log_file(config: &LauncherConfig, files: &[String]) -> Option<String> {
    config
        .selected_log_file
        .as_ref()
        .and_then(|selected| {
            files
                .iter()
                .find(|path| path.eq_ignore_ascii_case(selected))
        })
        .cloned()
        .or_else(|| files.first().cloned())
}

fn resolve_log_path(config: &LauncherConfig) -> LauncherResult<Option<PathBuf>> {
    let root = PathBuf::from(
        config
            .install_path
            .clone()
            .unwrap_or(default_install_path()?.to_string_lossy().into_owned()),
    );
    if let Some(relative) = config
        .selected_log_file
        .as_deref()
        .and_then(|path| normalized_relative_log_path(Path::new(path)))
    {
        let candidate = root.join(Path::new(&relative));
        if fs::symlink_metadata(&candidate).is_ok_and(|metadata| metadata.file_type().is_file()) {
            return Ok(Some(candidate));
        }
    }
    let files = discover_log_files(config)?;
    Ok(selected_log_file(config, &files).map(|relative| root.join(Path::new(&relative))))
}

fn list_log_files_sync() -> LauncherResult<Vec<LogFileOption>> {
    let mut config = load_config()?;
    if !config.developer_mode {
        return Err("Developer mode must be enabled to list logs.".to_string());
    }
    let files = discover_log_files(&config)?;
    let selected = selected_log_file(&config, &files);
    if config.selected_log_file != selected {
        config.selected_log_file = selected.clone();
        write_json(&config_path()?, &config)?;
    }
    Ok(files
        .into_iter()
        .map(|path| LogFileOption {
            name: path.clone(),
            selected: selected.as_ref() == Some(&path),
            path,
        })
        .collect())
}

#[tauri::command]
async fn list_log_files() -> LauncherResult<Vec<LogFileOption>> {
    tauri::async_runtime::spawn_blocking(list_log_files_sync)
        .await
        .map_err(error_string)?
}

#[tauri::command]
fn set_selected_log_file(path: String) -> LauncherResult<()> {
    let config = load_config()?;
    if !config.developer_mode {
        return Err("Developer mode must be enabled to select a log.".to_string());
    }
    let normalized = normalized_relative_log_path(Path::new(&path))
        .ok_or_else(|| "Invalid log file path.".to_string())?;
    let files = discover_log_files(&config)?;
    let selected = files
        .into_iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(&normalized))
        .ok_or_else(|| "The selected log file no longer exists.".to_string())?;
    save_selected_log_file(Some(selected))
}

#[tauri::command]
fn set_log_font_size(size: u8) -> LauncherResult<()> {
    save_log_font_size(size)
}

fn read_client_log_sync(offset: u64) -> LauncherResult<ClientLogChunk> {
    let config = load_config()?;
    if !config.developer_mode {
        return Err("Developer mode must be enabled to read the client log.".to_string());
    }
    let Some(log_path) = resolve_log_path(&config)? else {
        return Ok(ClientLogChunk {
            content: String::new(),
            next_offset: 0,
            reset: offset > 0,
            has_more: false,
        });
    };

    let mut file = File::open(log_path).map_err(error_string)?;
    let length = file.metadata().map_err(error_string)?.len();
    let reset = length < offset;
    let start = if reset { 0 } else { offset };
    file.seek(SeekFrom::Start(start)).map_err(error_string)?;
    let mut bytes = Vec::new();
    file.take(LOG_READ_CHUNK_SIZE)
        .read_to_end(&mut bytes)
        .map_err(error_string)?;
    Ok(ClientLogChunk {
        next_offset: start + bytes.len() as u64,
        content: String::from_utf8_lossy(&bytes).into_owned(),
        reset,
        has_more: start + (bytes.len() as u64) < length,
    })
}

#[tauri::command]
async fn read_client_log(offset: u64) -> LauncherResult<ClientLogChunk> {
    tauri::async_runtime::spawn_blocking(move || read_client_log_sync(offset))
        .await
        .map_err(error_string)?
}

fn clear_client_log_sync() -> LauncherResult<()> {
    let config = load_config()?;
    if !config.developer_mode {
        return Err("Developer mode must be enabled to clear the client log.".to_string());
    }
    let Some(log_path) = resolve_log_path(&config)? else {
        return Ok(());
    };
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(log_path)
        .map_err(error_string)?;
    Ok(())
}

#[tauri::command]
async fn clear_client_log() -> LauncherResult<()> {
    tauri::async_runtime::spawn_blocking(clear_client_log_sync)
        .await
        .map_err(error_string)?
}

#[tauri::command]
async fn choose_folder(
    app: AppHandle,
    state: State<'_, AppState>,
) -> LauncherResult<Option<LauncherSnapshot>> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let config = load_config()?;
        let current = PathBuf::from(config.install_path.unwrap());
        let installed = current.join(GAME_EXE).is_file()
            && is_managed_manifest(read_json::<Value>(&current.join(MANIFEST_FILE)).as_ref());
        let selected = rfd::FileDialog::new()
            .set_title(if installed {
                "Move OpenShores installation"
            } else {
                "Choose OpenShores install folder"
            })
            .set_directory(current.parent().unwrap_or(&current))
            .pick_folder();
        match selected {
            Some(path) => {
                if installed {
                    move_installation_sync(&app, &backend, current, path).map(Some)
                } else {
                    save_install_path(&path)?;
                    get_snapshot(&backend).map(Some)
                }
            }
            None => Ok(None),
        }
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
async fn install_game(
    app: AppHandle,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let backend = state.inner().clone();
    let install_path = PathBuf::from(load_config()?.install_path.unwrap());
    tauri::async_runtime::spawn_blocking(move || install_game_sync(&app, &backend, install_path))
        .await
        .map_err(error_string)?
}

#[tauri::command]
fn set_developer_mode(state: State<'_, AppState>, enabled: bool) -> LauncherResult<LauncherSnapshot> {
    save_developer_mode(enabled)?;
    get_snapshot(state.inner())
}

#[tauri::command]
fn set_post_launch_action(
    state: State<'_, AppState>,
    process: String,
    action: String,
) -> LauncherResult<LauncherSnapshot> {
    save_post_launch_action(&process, action)?;
    get_snapshot(state.inner())
}

fn unique_theme_file_name(requested: &str) -> LauncherResult<String> {
    let source_stem = Path::new(requested)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Imported Theme");
    let mut stem: String = source_stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, ' ' | '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    stem = stem.trim_matches(|character: char| character == ' ' || character == '-').to_string();
    if stem.is_empty() {
        stem = "Imported Theme".to_string();
    }
    stem.truncate(110);
    let requested = format!("{stem}.css");
    let directory = custom_themes_path()?;
    fs::create_dir_all(&directory).map_err(error_string)?;
    if !directory.join(&requested).exists() {
        return Ok(requested);
    }
    let stem = theme_name_from_file(&requested);
    for suffix in 2..10_000 {
        let candidate = format!("{stem} - {suffix}.css");
        if !directory.join(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err("Could not find an available theme filename.".to_string())
}

fn import_theme_css(requested_name: &str, css: String) -> LauncherResult<String> {
    validate_theme_css(&css)?;
    let file_name = unique_theme_file_name(requested_name)?;
    fs::write(custom_themes_path()?.join(&file_name), css).map_err(error_string)?;
    let id = format!("custom:{file_name}");
    save_selected_theme(id.clone())?;
    Ok(id)
}

#[tauri::command]
fn set_theme(state: State<'_, AppState>, theme_id: String) -> LauncherResult<LauncherSnapshot> {
    if !list_themes()?.iter().any(|theme| theme.id == theme_id) {
        return Err("Unknown theme.".to_string());
    }
    save_selected_theme(theme_id)?;
    get_snapshot(state.inner())
}

#[tauri::command]
async fn import_theme_file(state: State<'_, AppState>) -> LauncherResult<Option<LauncherSnapshot>> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let selected = rfd::FileDialog::new()
            .set_title("Import launcher theme")
            .add_filter("CSS theme", &["css"])
            .pick_file();
        let Some(path) = selected else { return Ok(None) };
        let metadata = fs::metadata(&path).map_err(error_string)?;
        if metadata.len() > MAX_THEME_SIZE as u64 {
            return Err("Themes may not be larger than 1 MB.".to_string());
        }
        let css = fs::read_to_string(&path).map_err(error_string)?;
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("Imported Theme.css");
        import_theme_css(name, css)?;
        get_snapshot(&backend).map(Some)
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
async fn import_theme_url(url: String, state: State<'_, AppState>) -> LauncherResult<LauncherSnapshot> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let parsed = Url::parse(url.trim()).map_err(|_| "Enter a valid HTTP or HTTPS URL.".to_string())?;
        if parsed.scheme() != "https" && parsed.scheme() != "http" {
            return Err("Theme URLs must use HTTP or HTTPS.".to_string());
        }
        let requested_name = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|segment| !segment.is_empty())
            .unwrap_or("Downloaded Theme.css");
        let requested_name = if requested_name.to_ascii_lowercase().ends_with(".css") {
            requested_name.to_string()
        } else {
            format!("{requested_name}.css")
        };
        let response = http_client()?.get(parsed).send().map_err(error_string)?.error_for_status().map_err(error_string)?;
        if response.content_length().is_some_and(|length| length > MAX_THEME_SIZE as u64) {
            return Err("Themes may not be larger than 1 MB.".to_string());
        }
        let bytes = response.bytes().map_err(error_string)?;
        if bytes.len() > MAX_THEME_SIZE {
            return Err("Themes may not be larger than 1 MB.".to_string());
        }
        let css = String::from_utf8(bytes.to_vec()).map_err(|_| "The downloaded theme is not valid UTF-8 CSS.".to_string())?;
        import_theme_css(&requested_name, css)?;
        get_snapshot(&backend)
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
fn delete_theme(state: State<'_, AppState>, theme_id: String) -> LauncherResult<LauncherSnapshot> {
    let file_name = theme_file_name(&theme_id).ok_or_else(|| "Built-in themes cannot be deleted.".to_string())?;
    let path = custom_themes_path()?.join(file_name);
    if path.is_file() {
        fs::remove_file(path).map_err(error_string)?;
    }
    if load_config()?.selected_theme == theme_id {
        save_selected_theme(default_theme())?;
    }
    get_snapshot(state.inner())
}

#[tauri::command]
async fn open_theme_editor(app: AppHandle, theme_id: Option<String>) -> LauncherResult<()> {
    if let Some(id) = theme_id.as_deref() {
        if theme_file_name(id).is_none() {
            return Err("Only custom themes can be edited.".to_string());
        }
    }
    if let Some(window) = app.get_webview_window("theme-editor") {
        let _ = window.show();
        let _ = window.set_focus();
        return Err("A theme editor is already open. Close it before opening another theme.".to_string());
    }
    let url = theme_id
        .map(|id| {
            let encoded = id.bytes().fold(String::new(), |mut output, byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                    output.push(byte as char);
                } else {
                    output.push_str(&format!("%{byte:02X}"));
                }
                output
            });
            format!("theme-editor.html?theme={encoded}")
        })
        .unwrap_or_else(|| "theme-editor.html".to_string());
    let webview_data = launcher_data_path()?.join("webview");
    let preview_app = app.clone();
    let editor = WebviewWindowBuilder::new(&app, "theme-editor", WebviewUrl::App(url.into()))
        .title("OpenShores Theme Editor")
        .inner_size(900.0, 680.0)
        .min_inner_size(640.0, 480.0)
        .center()
        .decorations(true)
        .resizable(true)
        .closable(true)
        .data_directory(webview_data)
        .build()
        .map_err(error_string)?;
    editor.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let _ = preview_app.emit_to("main", "theme-css-preview-clear", ());
        }
    });
    Ok(())
}

#[tauri::command]
async fn close_theme_editor(app: AppHandle) -> LauncherResult<()> {
    if let Some(window) = app.get_webview_window("theme-editor") {
        window.close().map_err(error_string)?;
    }
    Ok(())
}

#[tauri::command]
fn preview_theme_css(app: AppHandle, css: String) -> LauncherResult<()> {
    validate_theme_css(&css)?;
    app.emit_to("main", "theme-css-preview", css)
        .map_err(error_string)
}

#[tauri::command]
fn clear_theme_preview(app: AppHandle) -> LauncherResult<()> {
    app.emit_to("main", "theme-css-preview-clear", ())
        .map_err(error_string)
}

#[tauri::command]
async fn begin_theme_element_picker(app: AppHandle) -> LauncherResult<()> {
    if app.get_webview_window("theme-editor").is_none() {
        return Err("The theme editor is not open.".to_string());
    }
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "The launcher window is not available.".to_string())?;
    app.emit_to("main", "theme-element-picker-start", ())
        .map_err(error_string)?;
    let _ = main.unminimize();
    main.show().map_err(error_string)?;
    main.set_focus().map_err(error_string)
}

#[tauri::command]
async fn finish_theme_element_picker(
    app: AppHandle,
    selector: Option<String>,
) -> LauncherResult<()> {
    let editor = app
        .get_webview_window("theme-editor")
        .ok_or_else(|| "The theme editor was closed.".to_string())?;
    if let Some(selector) = selector {
        let selector = selector.trim();
        if selector.is_empty() || selector.len() > 1024 {
            return Err("The selected CSS selector is invalid.".to_string());
        }
        app.emit_to("theme-editor", "theme-element-selected", selector)
            .map_err(error_string)?;
    } else {
        app.emit_to("theme-editor", "theme-element-picker-cancelled", ())
            .map_err(error_string)?;
    }
    let _ = editor.show();
    editor.set_focus().map_err(error_string)
}

#[tauri::command]
fn get_theme_for_edit(theme_id: Option<String>) -> LauncherResult<ThemeEditorDocument> {
    match theme_id {
        Some(id) => {
            let file_name = theme_file_name(&id).ok_or_else(|| "Only custom themes can be edited.".to_string())?;
            Ok(ThemeEditorDocument {
                id: Some(id.clone()),
                name: theme_name_from_file(file_name),
                css: read_theme_css(&id)?,
            })
        }
        None => Ok(ThemeEditorDocument {
            id: None,
            name: "My Theme".to_string(),
            css: default_theme_template()?,
        }),
    }
}

#[tauri::command]
fn save_theme(
    app: AppHandle,
    state: State<'_, AppState>,
    original_id: Option<String>,
    name: String,
    css: String,
) -> LauncherResult<LauncherSnapshot> {
    validate_theme_css(&css)?;
    let file_name = theme_file_name_from_name(&name)?;
    let directory = custom_themes_path()?;
    fs::create_dir_all(&directory).map_err(error_string)?;
    let target = directory.join(&file_name);
    let old_file_name = match original_id.as_deref() {
        Some(id) => Some(theme_file_name(id).ok_or_else(|| "Only custom themes can be edited.".to_string())?),
        None => None,
    };
    if target.exists() && old_file_name != Some(file_name.as_str()) {
        return Err("A theme with that name already exists.".to_string());
    }
    fs::write(&target, css).map_err(error_string)?;
    if let Some(old_name) = old_file_name {
        if old_name != file_name {
            let old_path = directory.join(old_name);
            if old_path.is_file() {
                fs::remove_file(old_path).map_err(error_string)?;
            }
        }
    }
    let new_id = format!("custom:{file_name}");
    let selected = load_config()?.selected_theme;
    if original_id.is_none() || original_id.as_deref() == Some(selected.as_str()) {
        save_selected_theme(new_id)?;
    }
    let snapshot = get_snapshot(state.inner())?;
    let _ = app.emit("state-changed", snapshot.clone());
    Ok(snapshot)
}

#[tauri::command]
async fn uninstall_game(
    app: AppHandle,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || uninstall_game_sync(&app, &backend))
        .await
        .map_err(error_string)?
}

#[tauri::command]
fn add_server(
    nickname: String,
    host: String,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let host = normalize_server_host(&host)?;
    let mut config = load_config()?;
    let servers = config.servers.get_or_insert_with(Vec::new);
    if servers
        .iter()
        .any(|server| server.host.eq_ignore_ascii_case(&host))
    {
        return Err("That server is already in your list.".to_string());
    }
    let nickname = if nickname.trim().is_empty() {
        host.clone()
    } else {
        nickname.trim().to_string()
    };
    let id = format!(
        "server-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(error_string)?
            .as_nanos()
    );
    servers.push(ServerConfig { id, nickname, host });
    write_json(&config_path()?, &config)?;
    get_snapshot(state.inner())
}

#[tauri::command]
fn edit_server(
    server_id: String,
    nickname: String,
    host: String,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let host = normalize_server_host(&host)?;
    let mut config = load_config()?;
    let servers = config.servers.get_or_insert_with(Vec::new);
    if servers
        .iter()
        .any(|server| server.id != server_id && server.host.eq_ignore_ascii_case(&host))
    {
        return Err("That server is already in your list.".to_string());
    }
    let server = servers
        .iter_mut()
        .find(|server| server.id == server_id)
        .ok_or_else(|| "That server no longer exists.".to_string())?;
    let host_changed = !server.host.eq_ignore_ascii_case(&host);
    server.nickname = if nickname.trim().is_empty() {
        host.clone()
    } else {
        nickname.trim().to_string()
    };
    server.host = host;
    if host_changed {
        config
            .accounts
            .retain(|account| account.server_id != server_id);
        state
            .account_clients
            .lock()
            .map_err(error_string)?
            .remove(&server_id);
    }
    write_json(&config_path()?, &config)?;
    state
        .server_statuses
        .lock()
        .map_err(error_string)?
        .remove(&server_id);
    get_snapshot(state.inner())
}

#[tauri::command]
fn remove_server(
    server_id: String,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let mut config = load_config()?;
    let servers = config.servers.get_or_insert_with(Vec::new);
    let original_len = servers.len();
    servers.retain(|server| server.id != server_id);
    if servers.len() == original_len {
        return Err("That server no longer exists.".to_string());
    }
    if config.connected_server_id.as_deref() == Some(server_id.as_str()) {
        config.connected_server_id = None;
    }
    config
        .accounts
        .retain(|account| account.server_id != server_id);
    state
        .account_clients
        .lock()
        .map_err(error_string)?
        .remove(&server_id);
    state
        .server_statuses
        .lock()
        .map_err(error_string)?
        .remove(&server_id);
    write_json(&config_path()?, &config)?;
    get_snapshot(state.inner())
}

#[tauri::command]
async fn connect_server(
    server_id: Option<String>,
    force: bool,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut config = load_config()?;
        if let Some(id) = server_id.as_deref() {
            let server = server_by_id(&config, id)?;
            if force {
                if !config.developer_mode {
                    return Err("Developer mode must be enabled to force a connection.".to_string());
                }
            } else {
                let client = Client::builder()
                    .user_agent(format!(
                        "OpenShores-Launcher/{}",
                        env!("OPENSHORES_LAUNCHER_VERSION")
                    ))
                    .timeout(Duration::from_secs(5))
                    .build()
                    .map_err(error_string)?;
                let status = fetch_server_status(&client, &server);
                backend
                    .server_statuses
                    .lock()
                    .map_err(error_string)?
                    .insert(server.id, status.clone());
                if !status.online() {
                    return Err(
                        "This server is unavailable or offline. Refresh its status and try again."
                            .to_string(),
                    );
                }
            }
        }
        config.connected_server_id = server_id;
        write_json(&config_path()?, &config)?;
        get_snapshot(&backend)
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
async fn refresh_server_status(
    server_id: String,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let config = load_config()?;
        let server = server_by_id(&config, &server_id)?;
        let client = Client::builder()
            .user_agent(format!(
                "OpenShores-Launcher/{}",
                env!("OPENSHORES_LAUNCHER_VERSION")
            ))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(error_string)?;
        let status = fetch_server_status(&client, &server);
        backend
            .server_statuses
            .lock()
            .map_err(error_string)?
            .insert(server.id, status);
        get_snapshot(&backend)
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
async fn refresh_server_statuses(state: State<'_, AppState>) -> LauncherResult<LauncherSnapshot> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let servers = load_config()?.servers.unwrap_or_default();
        let client = Client::builder()
            .user_agent(format!(
                "OpenShores-Launcher/{}",
                env!("OPENSHORES_LAUNCHER_VERSION")
            ))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(error_string)?;
        let handles: Vec<_> = servers
            .into_iter()
            .map(|server| {
                let client = client.clone();
                thread::spawn(move || {
                    let status = fetch_server_status(&client, &server);
                    (server.id, status)
                })
            })
            .collect();
        let mut statuses = backend.server_statuses.lock().map_err(error_string)?;
        for handle in handles {
            let (id, status) = handle
                .join()
                .map_err(|_| "A server status check failed.".to_string())?;
            statuses.insert(id, status);
        }
        drop(statuses);
        get_snapshot(&backend)
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
async fn check_path(cmd: String, _state: State<'_, AppState>) -> LauncherResult<PathPayload> {
    if cmd != WINE_NAME && cmd != HPATCHZ_NAME {
        return Err("Checking for this command is not allowed.".to_string())
    }
    tauri::async_runtime::spawn_blocking(move || {
        let path = which(cmd).map_err(error_string)?;
        let serialized = path.to_str().ok_or("Invalid path")?;

        Ok(PathPayload { path: serialized.to_string() })
    })
    .await
    .map_err(error_string)?
}

fn authenticate_account(
    state: &AppState,
    endpoint: &str,
    username: String,
    password: String,
) -> LauncherResult<LauncherSnapshot> {
    let mut config = load_config()?;
    let server = connected_server(&config)?;
    if endpoint == "register" {
        return Err(format!(
            "Create your account on {} first, then log in here.",
            account_domain(&server.host)
        ));
    }
    let client = account_client()?;
    let response = client
        .post(server_api_url(&server.host, "launcher/login")?)
        .header(LAUNCHER_HEADER, "1")
        .json(&json!({ "username": username.trim(), "password": password }))
        .send()
        .map_err(|_| "Could not reach the account server.".to_string())?;
    if !response.status().is_success() {
        return Err(response_error(
            response,
            if endpoint == "register" {
                "Registration failed"
            } else {
                "Login failed"
            },
        ));
    }
    let session = response
        .cookies()
        .find(|cookie| cookie.name() == SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string())
        .filter(|value| is_session_value(value));
    let canonical_username = response
        .json::<Value>()
        .ok()
        .and_then(|value| {
            value
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| username.trim().to_string());
    let saved_account = SavedAccount {
        server_id: server.id.clone(),
        username: canonical_username,
        password_sha1: password_sha1_hex(&password),
        session,
    };
    config
        .accounts
        .retain(|account| account.server_id != server.id);
    config.accounts.push(saved_account);
    write_json(&config_path()?, &config)?;
    state
        .account_clients
        .lock()
        .map_err(error_string)?
        .insert(server.id, client);
    get_snapshot(state)
}

#[tauri::command]
async fn login_account(
    username: String,
    password: String,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    if username.trim().is_empty() || password.is_empty() {
        return Err("Enter your username and password.".to_string());
    }
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        authenticate_account(&backend, "login", username, password)
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
async fn register_account(
    username: String,
    password: String,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    if username.trim().is_empty() || password.is_empty() {
        return Err("Enter a username and password.".to_string());
    }
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        authenticate_account(&backend, "register", username, password)
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
async fn logout_account(state: State<'_, AppState>) -> LauncherResult<LauncherSnapshot> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut config = load_config()?;
        if let Ok(server) = connected_server(&config) {
            let client = backend
                .account_clients
                .lock()
                .map_err(error_string)?
                .remove(&server.id);
            if let Some(client) = client {
                if let Ok(url) = server_api_url(&server.host, "logout") {
                    let _ = client.post(url).send();
                }
            }
            config
                .accounts
                .retain(|account| account.server_id != server.id);
            write_json(&config_path()?, &config)?;
        }
        get_snapshot(&backend)
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
fn launch_game(app: AppHandle, state: State<'_, AppState>) -> LauncherResult<LauncherSnapshot> {
    launch_game_sync(&app, state.inner(), None)
}

fn designer_launch_argument(mode: &str) -> LauncherResult<&'static str> {
    match mode {
        "spacecraft" => Ok("-spacecraft"),
        "building" => Ok("-building"),
        "designer" => Ok("-designer"),
        _ => Err("Unknown offline designer mode.".to_string()),
    }
}

#[tauri::command]
fn launch_offline_designer(
    mode: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    launch_game_sync(&app, state.inner(), Some(&mode))
}

#[tauri::command]
fn stop_game_process(
    process: String,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let tracked_pid = match process.as_str() {
        "game" => state.game_pid.clone(),
        "designer" => state.designer_pid.clone(),
        _ => return Err("Unknown game process.".to_string()),
    };
    let pid = *tracked_pid.lock().map_err(error_string)?;
    let Some(pid) = pid else {
        return get_snapshot(state.inner());
    };

    #[cfg(windows)]
    let status = Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(error_string)?;

    #[cfg(not(windows))]
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(error_string)?;

    if !status.success() {
        return Err("The running process could not be stopped.".to_string());
    }
    if let Ok(mut current) = tracked_pid.lock() {
        if *current == Some(pid) {
            *current = None;
        }
    }
    get_snapshot(state.inner())
}

#[tauri::command]
fn minimize_launcher(app: AppHandle) -> LauncherResult<()> {
    app.get_webview_window("main")
        .ok_or_else(|| "Launcher window is unavailable.".to_string())?
        .minimize()
        .map_err(error_string)
}

#[tauri::command]
fn exit_launcher(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn open_folder() -> LauncherResult<()> {
    let path = PathBuf::from(load_config()?.install_path.unwrap());
    open::that(path).map_err(error_string)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct NewsItem {
    title: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    summary: String,
    url: String,
}

fn fetch_news() -> Vec<NewsItem> {
    let Ok(config) = load_config() else { return Vec::new() };
    let Ok(server) = connected_server(&config) else { return Vec::new() };
    let Ok(url) = server_api_url(&server.host, "news") else { return Vec::new() };
    let Ok(client) = http_client() else { return Vec::new() };
    let site = format!("https://{}", account_domain(&server.host));
    client
        .get(url)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .ok()
        .filter(|response| response.status().is_success())
        .and_then(|response| response.json::<Vec<NewsItem>>().ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|item| news_path_ok(&item.url) && !item.title.trim().is_empty())
        .take(NEWS_ITEMS)
        .map(|item| NewsItem {
            title: item.title.chars().take(160).collect(),
            date: item.date.chars().take(10).collect(),
            summary: item.summary.chars().take(300).collect(),
            url: format!("{site}{}", item.url),
        })
        .collect()
}

fn news_path_ok(path: &str) -> bool {
    path.strip_prefix("/news/").is_some_and(|slug| {
        !slug.is_empty()
            && slug.len() <= 100
            && slug.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    })
}

#[tauri::command]
async fn get_news() -> LauncherResult<Vec<NewsItem>> {
    tauri::async_runtime::spawn_blocking(fetch_news)
        .await
        .map_err(error_string)
}

#[tauri::command]
fn open_link(url: String) -> LauncherResult<()> {
    let site = load_config()
        .ok()
        .and_then(|config| connected_server(&config).ok())
        .map(|server| format!("https://{}/", account_domain(&server.host)))
        .unwrap_or_default();
    let allowed = [
        "https://openshores.net/",
        "https://github.com/Norway174/OpenShores-Launcher",
        site.as_str(),
    ];
    if !allowed.iter().any(|prefix| !prefix.is_empty() && url.starts_with(prefix)) {
        return Err("Blocked external URL.".to_string());
    }
    open::that(url).map_err(error_string)
}

#[tauri::command]
fn get_changelog(state: State<'_, AppState>) -> LauncherResult<ChangelogPayload> {
    state
        .changelog
        .lock()
        .map_err(error_string)?
        .clone()
        .ok_or_else(|| "No changelog is available.".to_string())
}

#[tauri::command]
async fn open_changelog(
    app: AppHandle,
    state: State<'_, AppState>,
    title: String,
    changelog: String,
    release_url: String,
) -> LauncherResult<()> {
    if title.trim().is_empty() || changelog.trim().is_empty() {
        return Err("No changelog is available for this update.".to_string());
    }
    let site = load_config()
        .ok()
        .and_then(|config| connected_server(&config).ok())
        .map(|server| format!("https://{}/", account_domain(&server.host)))
        .unwrap_or_default();
    let known = ["https://github.com/", "https://openshores.net/", site.as_str()];
    if !known.iter().any(|prefix| !prefix.is_empty() && release_url.starts_with(prefix)) {
        return Err("The release URL is not a known address.".to_string());
    }
    *state.changelog.lock().map_err(error_string)? = Some(ChangelogPayload {
        title,
        changelog,
        release_url,
    });
    if let Some(window) = app.get_webview_window("changelog") {
        let _ = window.show();
        let _ = window.set_focus();
        window.eval("window.location.reload()").map_err(error_string)?;
        return Ok(());
    }
    let webview_data = launcher_data_path()?.join("webview");
    WebviewWindowBuilder::new(&app, "changelog", WebviewUrl::App("changelog.html".into()))
        .title("OpenShores Changelog")
        .inner_size(720.0, 640.0)
        .min_inner_size(520.0, 420.0)
        .center()
        .decorations(true)
        .resizable(true)
        .closable(true)
        .data_directory(webview_data)
        .build()
        .map_err(error_string)?;
    Ok(())
}

#[tauri::command]
async fn close_changelog(app: AppHandle) -> LauncherResult<()> {
    if let Some(window) = app.get_webview_window("changelog") {
        window.close().map_err(error_string)?;
    }
    Ok(())
}

#[tauri::command]
async fn check_updates(
    app: AppHandle,
    state: State<'_, AppState>,
) -> LauncherResult<UpdaterStatusPayload> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || check_updates_sync(&app, &backend, true))
        .await
        .map_err(error_string)?
}

#[tauri::command]
async fn check_game_update() -> LauncherResult<UpdaterStatusPayload> {
    tauri::async_runtime::spawn_blocking(check_game_update_sync)
        .await
        .map_err(error_string)?
}

fn install_launcher_update_sync(app: &AppHandle, state: &AppState) -> LauncherResult<bool> {
    let pending = state
        .pending_update
        .lock()
        .map_err(error_string)?
        .clone()
        .ok_or_else(|| "No launcher update is available.".to_string())?;
    let client = http_client()?;
    let destination = download_launcher_update(app, &client, &pending.asset, &pending.version)?;
    emit_updater(app, "installing", format!("Installing launcher {}...", pending.version), Some(100));

    #[cfg(windows)]
    {
        let target = env::current_exe().map_err(error_string)?;
        let temp = launcher_temp_path()?;
        fs::create_dir_all(&temp).map_err(error_string)?;
        let batch_path = temp.join("apply-launcher-update.bat");
        let error_log = launcher_data_path()?.join("update-error.log");
        let script = [
            "@echo off".to_string(),
            "setlocal".to_string(),
            format!("set \"LAUNCHER_PID={}\"", std::process::id()),
            format!("set \"UPDATE_SOURCE={}\"", batch_escape(&destination)),
            format!("set \"UPDATE_TARGET={}\"", batch_escape(&target)),
            format!("set \"ERROR_LOG={}\"", batch_escape(&error_log)),
            format!("rem Updating to {}", pending.version),
            ":wait_for_launcher".to_string(),
            "tasklist /FI \"PID eq %LAUNCHER_PID%\" 2>NUL | find \"%LAUNCHER_PID%\" >NUL".to_string(),
            "if not errorlevel 1 ( ping 127.0.0.1 -n 2 >NUL & goto wait_for_launcher )".to_string(),
            "set \"REPLACE_ATTEMPTS=0\"".to_string(),
            ":replace_launcher".to_string(),
            "copy /Y \"%UPDATE_SOURCE%\" \"%UPDATE_TARGET%\" >NUL 2>&1".to_string(),
            "if not errorlevel 1 goto replacement_complete".to_string(),
            "set /A REPLACE_ATTEMPTS+=1".to_string(),
            "if %REPLACE_ATTEMPTS% GEQ 30 goto replacement_failed".to_string(),
            "ping 127.0.0.1 -n 2 >NUL".to_string(),
            "goto replace_launcher".to_string(),
            ":replacement_failed".to_string(),
            "echo The launcher update could not replace the portable executable.>\"%ERROR_LOG%\""
                .to_string(),
            "start \"\" \"%UPDATE_TARGET%\"".to_string(),
            "exit /b 1".to_string(),
            ":replacement_complete".to_string(),
            "del /Q \"%UPDATE_SOURCE%\" >NUL 2>&1".to_string(),
            "del /Q \"%ERROR_LOG%\" >NUL 2>&1".to_string(),
            "start \"\" \"%UPDATE_TARGET%\"".to_string(),
            "(goto) 2>NUL & del \"%~f0\"".to_string(),
            String::new(),
        ]
        .join("\r\n");
        fs::write(&batch_path, script).map_err(error_string)?;
        let mut command = Command::new("cmd.exe");
        command
            .args(["/d", "/c"])
            .arg(&batch_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        command.spawn().map_err(error_string)?;
        let app = app.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(250));
            app.exit(0);
        });
    }

    #[cfg(target_os = "linux")]
    {
        // Can't use current_exe() b/c we're in an AppImage, $APPIMAGE is
        // set to the path of the actual AppImage file.
        let target = env::var("APPIMAGE").map_err(error_string)?;

        // Make executable
        println!("destination: {destination:?}");
        println!("target: {target:?}");
        let metadata = fs::metadata(&destination).map_err(error_string)?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(&destination, permissions).map_err(error_string)?;

        fs::rename(&destination, &target).map_err(error_string)?;

        // Replace current process with new version
        let error = Command::new(&target).exec();
        return Err(error_string(error));
    }
    Ok(true)
}

#[tauri::command]
async fn install_launcher_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> LauncherResult<bool> {
    let backend = state.inner().clone();
    let app_for_work = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        install_launcher_update_sync(&app_for_work, &backend)
    })
    .await
    .map_err(error_string)?;
    if let Err(error) = &result {
        emit_updater(&app, "error", format!("Launcher update failed: {error}"), None);
    }
    result
}

fn error_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn main() {
    // Workaround for current Tauri versions not working with Gtk + some Nvidia drivers on Linux
    #[cfg(target_os = "linux")]
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_os::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_state,
            list_log_files,
            set_selected_log_file,
            set_log_font_size,
            read_client_log,
            clear_client_log,
            choose_folder,
            install_game,
            set_developer_mode,
            set_post_launch_action,
            set_theme,
            import_theme_file,
            import_theme_url,
            delete_theme,
            open_theme_editor,
            close_theme_editor,
            preview_theme_css,
            clear_theme_preview,
            begin_theme_element_picker,
            finish_theme_element_picker,
            get_theme_for_edit,
            save_theme,
            uninstall_game,
            add_server,
            edit_server,
            remove_server,
            connect_server,
            refresh_server_status,
            refresh_server_statuses,
            login_account,
            register_account,
            logout_account,
            launch_game,
            launch_offline_designer,
            stop_game_process,
            minimize_launcher,
            exit_launcher,
            open_folder,
            open_link,
            get_changelog,
            get_news,
            open_changelog,
            close_changelog,
            check_updates,
            check_game_update,
            install_launcher_update,
            check_path
        ])
        .setup(|app| {
            cleanup_legacy_electron_data()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
            let webview_data = launcher_data_path()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?
                .join("webview");
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("OpenShores Launcher")
                .inner_size(800.0, 640.0)
                .min_inner_size(700.0, 560.0)
                .center()
                .decorations(true)
                .maximizable(false)
                .transparent(false)
                .shadow(true)
                .data_directory(webview_data)
                .build()?;
            let app_handle = app.handle().clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(1));
                report_previous_update_error(&app_handle);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("OpenShores Launcher failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_create_safe_css_file_names() {
        assert_eq!(theme_file_name_from_name("Ocean Blue").unwrap(), "Ocean Blue.css");
        assert_eq!(theme_file_name_from_name("Ocean.css").unwrap(), "Ocean.css");
        assert!(theme_file_name_from_name("../outside").is_err());
        assert!(theme_file_name("custom:../outside.css").is_none());
        assert!(theme_file_name("default-dark").is_none());
    }

    #[test]
    fn custom_theme_css_is_size_limited() {
        assert!(validate_theme_css(":root { --blue: red; }").is_ok());
        assert!(validate_theme_css("bad\0css").is_err());
        assert!(validate_theme_css(&"x".repeat(MAX_THEME_SIZE + 1)).is_err());
    }

    #[test]
    fn new_theme_template_comes_from_default_dark_root_rule() {
        let template = default_theme_template().unwrap();
        assert!(template.starts_with(":root {"));
        assert!(template.contains("--window: #262626;"));
        assert!(template.contains("--blue-hover: #3293c2;"));
        assert!(template.ends_with("}\n"));
    }

    #[test]
    fn update_batch_paths_escape_percent_expansion() {
        assert_eq!(
            batch_escape(Path::new(r"C:\Temp\100%\Launcher.exe")),
            r"C:\Temp\100%%\Launcher.exe"
        );
    }

    #[test]
    fn launcher_updates_require_a_github_checksum() {
        let asset = GithubAsset {
            name: "OpenShores-Launcher.exe".to_string(),
            browser_download_url: "https://example.invalid/launcher.exe".to_string(),
            digest: None,
            size: 0,
        };
        let error = verify_launcher_digest(Path::new("unused"), &asset).unwrap_err();
        assert!(error.contains("checksum"));
    }

    #[cfg(windows)]
    #[test]
    fn offline_designer_modes_map_to_expected_arguments() {
        assert_eq!(designer_launch_argument("spacecraft").unwrap(), "-spacecraft");
        assert_eq!(designer_launch_argument("building").unwrap(), "-building");
        assert_eq!(designer_launch_argument("designer").unwrap(), "-designer");
        assert!(designer_launch_argument("unknown").is_err());
    }

    #[test]
    fn developer_game_launch_adds_log_argument() {
        assert!(main_game_launch_arguments(false).is_empty());
        assert_eq!(main_game_launch_arguments(true), vec!["-log"]);
    }

    #[test]
    fn log_file_selections_only_accept_relative_log_and_text_paths() {
        assert_eq!(
            normalized_relative_log_path(Path::new(r"Logs\ClientInterface.log")).as_deref(),
            Some("Logs/ClientInterface.log")
        );
        assert_eq!(
            normalized_relative_log_path(Path::new("diagnostics.txt")).as_deref(),
            Some("diagnostics.txt")
        );
        assert!(normalized_relative_log_path(Path::new("../outside.log")).is_none());
        assert!(normalized_relative_log_path(Path::new("notes.json")).is_none());
    }

    #[test]
    fn log_discovery_ignores_patch_originals_and_example_story_directories() {
        assert!(ignored_log_directory(Path::new(PATCH_ORIGINALS_DIR)));
        assert!(ignored_log_directory(Path::new("ExampleStory")));
        assert!(ignored_log_directory(Path::new("examplestory")));
        assert!(!ignored_log_directory(Path::new("Story")));
    }

    #[test]
    fn empty_server_lists_are_distinct_from_uninitialized_configs() {
        let config: LauncherConfig = serde_json::from_str(r#"{"servers":[]}"#).unwrap();
        assert_eq!(config.servers, Some(Vec::new()));
    }

    #[test]
    fn account_endpoints_drop_the_play_subdomain() {
        assert_eq!(account_domain("play.openshores.net"), "openshores.net");
        assert_eq!(account_domain("play.example.co.uk"), "example.co.uk");
        assert_eq!(
            server_api_url("play.openshores.net", "login")
                .unwrap()
                .as_str(),
            "https://openshores.net/api/login"
        );
    }

    #[test]
    fn hazeron_password_uses_raw_sha1_bytes_in_qt_byte_array_format() {
        let stored_hash = password_sha1_hex("password");
        assert_eq!(stored_hash, "5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8");
        let value = hazeron_password_value_from_hex(&stored_hash).unwrap();
        assert_eq!(value.chars().count(), 32);
        let payload_hex: String = value
            .chars()
            .skip(11)
            .take(20)
            .map(|character| format!("{:02x}", character as u32))
            .collect();
        assert_eq!(payload_hex, "5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8");
    }

    #[test]
    fn wine_password_is_properly_formed() {
        let value = wine_password_value_from_hex("79558f0f0033e4bcc76a1bd01b7106ae7e837074").unwrap();
        assert_eq!(value, "40,00,42,00,79,00,74,00,65,00,41,00,72,00,72,00,61,00,79,00,28,00,79,00,55,00,8f,00,0f,00,00,00,33,00,e4,00,bc,00,c7,00,6a,00,1b,00,d0,00,1b,00,71,00,06,00,ae,00,7e,00,83,00,70,00,74,00,29,00")
    }

    #[test]
    fn wine_password_import_is_utf16_reg_sz_data() {
        let value = wine_password_value_from_hex(
            "000102030405060708090a0b0c0d0e0f10111213",
        )
        .unwrap();
        let import = wine_password_registry_import(
            r"HKEY_USERS\S-1-5-21-0-0-0-1000\Software\Software Engineering\Shores of Hazeron\Account",
            &value,
        );

        assert_eq!(&import[..2], &[0xff, 0xfe]);
        let text = String::from_utf16(
            &import[2..]
                .chunks_exact(2)
                .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(text.starts_with("Windows Registry Editor Version 5.00\r\n"));
        assert!(text.contains("\"Password\"=hex:40,00,42,00"));
        assert!(text.ends_with("29,00\r\n"));
    }

    #[cfg(windows)]
    #[test]
    fn account_registry_path_is_relative_to_the_current_user() {
        assert!(ACCOUNT_REGISTRY_PATH.starts_with(r"Software\"));
        assert!(!ACCOUNT_REGISTRY_PATH.contains("S-1-5-"));
    }

    #[test]
    fn saved_accounts_store_only_the_compatible_sha1_digest() {
        let account = SavedAccount {
            server_id: "openshores".to_string(),
            username: "Explorer".to_string(),
            password_sha1: password_sha1_hex("secret"),
            session: None,
        };
        let json = serde_json::to_string(&account).unwrap();
        assert!(json.contains("e5e9fa1ba31ecd1ae84f75caaa474f3a663f05f4"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn the_website_session_is_kept_only_when_it_is_a_plain_key() {
        assert!(is_session_value("0123456789abcdefghijklmnopqrstuv"));
        for bad in ["", "short", "abc; other=1 0123456789abcdef", "0123456789abcdef
X: y"] {
            assert!(!is_session_value(bad), "{bad}");
        }
        let account = SavedAccount {
            server_id: "openshores".to_string(),
            username: "Explorer".to_string(),
            password_sha1: password_sha1_hex("secret"),
            session: Some("0123456789abcdefghijklmnopqrstuv".to_string()),
        };
        assert!(site_client(&account).is_ok());
        let logged_out: SavedAccount = serde_json::from_str(
            r#"{"serverId":"openshores","username":"Explorer","passwordSha1":"e5e9fa1ba31ecd1ae84f75caaa474f3a663f05f4"}"#,
        )
        .unwrap();
        assert_eq!(logged_out.session, None);
        assert_eq!(site_client(&logged_out).unwrap_err(), LOGIN_NEEDED);
    }

    #[test]
    fn a_launcher_offer_is_checked_and_compared() {
        let offer = |version: &str, url: &str, sha: &str| LauncherOffer {
            version: version.to_string(),
            size: 1,
            sha256: sha.to_string(),
            url: url.to_string(),
            notes: String::new(),
        };
        let sha = "ab".repeat(32);
        assert!(offer("2.0.1", "/api/launcher/download", &sha).usable());
        for (version, url, digest) in [
            ("", "/api/launcher/download", sha.as_str()),
            ("2.0.1 beta", "/api/launcher/download", sha.as_str()),
            ("2.0.1", "https://elsewhere.example/launcher.exe", sha.as_str()),
            ("2.0.1", "/api/../secret", sha.as_str()),
            ("2.0.1", "/api/launcher/download", "short"),
        ] {
            assert!(!offer(version, url, digest).usable(), "{version} {url}");
        }
        assert!(offer("2.0.1", "/api/x", &sha).newer_than("2.0.0"));
        assert!(offer("2.1.0", "/api/x", &sha).newer_than("2.0.9"));
        assert!(!offer("2.0.0", "/api/x", &sha).newer_than("2.0.0"));
        assert!(!offer("1.9.9", "/api/x", &sha).newer_than("2.0.0"));
        assert!(offer("spring", "/api/x", &sha).newer_than("2.0.0"));
        assert!(!offer("spring", "/api/x", &sha).newer_than("Spring"));
    }

    #[test]
    fn news_links_stay_on_the_news_pages() {
        assert!(news_path_ok("/news/first-flight"));
        for bad in ["/news/", "/news/a/b", "/account", "https://elsewhere.example/news/x", "/news/../admin", "/news/a?b"] {
            assert!(!news_path_ok(bad), "{bad}");
        }
    }

    #[test]
    fn a_part_already_downloaded_is_not_counted_again() {
        let folder = env::temp_dir().join(format!("openshores-launcher-test-parts-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        assert_eq!(bytes_to_download(&folder, "p1.zip", 100), 100);
        fs::write(folder.join("p1.zip.part"), [0_u8; 40]).unwrap();
        assert_eq!(bytes_to_download(&folder, "p1.zip", 100), 60);
        fs::write(folder.join("p1.zip"), [0_u8; 100]).unwrap();
        assert_eq!(bytes_to_download(&folder, "p1.zip", 100), 0);
        assert!(require_free_space(&folder, 1, "for the test").is_ok());
        let error = require_free_space(&folder, u64::MAX, "for the test").unwrap_err();
        assert!(error.contains("Not enough free disk space for the test"), "{error}");
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn sizes_are_shown_in_the_unit_that_fits() {
        assert_eq!(format_bytes(512 * 1024), "512.0 KB");
        assert_eq!(format_bytes(245 * 1024 * 1024), "245.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.50 GB");
    }

    #[test]
    fn the_patcher_is_embedded() {
        #[cfg(windows)]
        assert!(install::HPATCHZ_BYTES.is_some_and(|bytes| bytes.len() > 100_000));
    }

    #[test]
    fn multiple_servers_keep_independent_saved_accounts() {
        let mut config = LauncherConfig::default();
        config.accounts = vec![
            SavedAccount {
                server_id: "openshores".to_string(),
                username: "Explorer".to_string(),
                password_sha1: password_sha1_hex("first"),
                session: None,
            },
            SavedAccount {
                server_id: "community".to_string(),
                username: "Builder".to_string(),
                password_sha1: password_sha1_hex("second"),
                session: None,
            },
        ];
        let username_for = |server_id: &str| {
            config
                .accounts
                .iter()
                .find(|account| account.server_id == server_id)
                .map(|account| account.username.as_str())
        };
        assert_eq!(username_for("openshores"), Some("Explorer"));
        assert_eq!(username_for("community"), Some("Builder"));
    }

    #[test]
    fn installation_size_walk_includes_nested_files() {
        let root = env::temp_dir().join(format!(
            "openshores-move-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("one.bin"), [0u8; 3]).unwrap();
        fs::write(nested.join("two.bin"), [0u8; 7]).unwrap();
        assert_eq!(directory_file_bytes(&root).unwrap(), 10);
        fs::remove_dir_all(root).unwrap();
    }

}
