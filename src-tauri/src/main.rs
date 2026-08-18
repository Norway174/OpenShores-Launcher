#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use psl::{List, Psl};
use reqwest::{
    blocking::{Client, Response},
    Url,
};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use semver::Version;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
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
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use which::which;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;

const GAME_MANIFEST_URL: &str = "https://openshores.net/downloads/manifest.json";
const PATCH_RELEASE_API: &str =
    "https://api.github.com/repos/Celarious/OpenShores-IP-Patch/releases?per_page=100";
const LAUNCHER_RELEASE_API: &str =
    "https://api.github.com/repos/Norway174/OpenShores-Launcher/releases/latest";
const GAME_EXE: &str = "Shores of Hazeron.exe";
const GAME_DLL: &str = "AuLoginClient13.dll";
const CLIENT_LOG: &str = "ClientInterface.log";
const MANIFEST_FILE: &str = ".openshores-launcher.json";
const MANAGED_BY: &str = "OpenShores Launcher";
const LATEST_PATCH_RELEASE: &str = "latest";
const DISABLED_PATCH_RELEASE: &str = "none";
const PATCH_ORIGINALS_DIR: &str = ".openshores-patch-originals";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const XDELTA_TIMEOUT: Duration = Duration::from_secs(120);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DEFAULT_SERVER_HOST: &str = "play.openshores.net";
const WINE_NAME: &str = "wine";
const DEFAULT_DARK_THEME: &str = "default-dark";
const DEFAULT_LIGHT_THEME: &str = "default-light";
const POST_LAUNCH_DO_NOTHING: &str = "do_nothing";
const POST_LAUNCH_MINIMIZE: &str = "minimize";
const POST_LAUNCH_EXIT: &str = "exit";
const POST_LAUNCH_OPEN_LOGS: &str = "open_logs";
const MAX_THEME_SIZE: usize = 1024 * 1024;
const DEFAULT_DARK_CSS: &str = include_str!("../../src/renderer/styles.css");

// OS-specific constants
#[cfg(windows)]
const XDELTA_NAME: &str = "xdelta3-3.0.11-x86_64.exe";
#[cfg(windows)]
const XDELTA_BYTES: &[u8] = include_bytes!("../../resources/xdelta3/xdelta3-3.0.11-x86_64.exe");
#[cfg(windows)]
const ACCOUNT_REGISTRY_PATH: &str = r"Software\Software Engineering\Shores of Hazeron\Account";
#[cfg(target_os = "linux")]
const XDELTA_NAME: &str = "xdelta3";
#[cfg(target_os = "linux")]
const ACCOUNT_REGISTRY_PATH: &str = r"S-1-5-21-0-0-0-1000\Software\Software Engineering\Shores of Hazeron\Account";

type LauncherResult<T> = Result<T, String>;

#[derive(Clone, Default)]
struct AppState {
    active_operation: Arc<AtomicBool>,
    game_pid: Arc<Mutex<Option<u32>>>,
    designer_pid: Arc<Mutex<Option<u32>>>,
    pending_update: Arc<Mutex<Option<PendingUpdate>>>,
    server_statuses: Arc<Mutex<HashMap<String, ServerStatus>>>,
    account_clients: Arc<Mutex<HashMap<String, Client>>>,
}

#[derive(Clone)]
struct PendingUpdate {
    asset: GithubAsset,
    version: Version,
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
    #[serde(rename = "ipPatchRelease", default = "default_patch_release")]
    ip_patch_release: String,
    #[serde(
        rename = "appliedIpPatchRelease",
        skip_serializing_if = "Option::is_none"
    )]
    applied_ip_patch_release: Option<String>,
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
            ip_patch_release: default_patch_release(),
            applied_ip_patch_release: None,
            servers: Some(vec![default_server()]),
            connected_server_id: None,
            accounts: Vec::new(),
            developer_mode: false,
            selected_theme: default_theme(),
            game_launch_action: default_post_launch_action(),
            designer_launch_action: default_post_launch_action(),
        }
    }
}

fn default_theme() -> String {
    DEFAULT_DARK_THEME.to_string()
}

fn default_patch_release() -> String {
    LATEST_PATCH_RELEASE.to_string()
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
    ip_patch_release: String,
    applied_ip_patch_release: Option<String>,
    servers: Vec<ServerSnapshot>,
    connected_server_id: Option<String>,
    account_username: Option<String>,
    developer_mode: bool,
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
#[serde(rename_all = "camelCase")]
struct PatchReleaseOption {
    tag: String,
    name: String,
    published_at: Option<String>,
    has_zip: bool,
    prerelease: bool,
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
struct UpdaterStatusPayload {
    state: String,
    message: String,
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
}

#[derive(Clone, Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    #[serde(default)]
    size: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct GameManifest {
    version: String,
    generated: u64,
    base_url: String,
    total_size: u64,
    files: Vec<GameManifestFile>,
}

#[derive(Clone, Debug, Deserialize)]
struct GameManifestFile {
    path: String,
    size: u64,
    sha256: String,
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

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> LauncherResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(error_string)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(error_string)?;
    fs::write(path, text).map_err(error_string)
}

fn path_eq(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn is_managed_manifest(manifest: Option<&Value>) -> bool {
    manifest
        .and_then(|value| value.get("managedBy"))
        .and_then(Value::as_str)
        == Some(MANAGED_BY)
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

fn save_patch_selection(selection: String) -> LauncherResult<LauncherConfig> {
    let mut config = load_config()?;
    config.ip_patch_release = if selection.trim().is_empty() {
        default_patch_release()
    } else {
        selection
    };
    write_json(&config_path()?, &config)?;
    Ok(config)
}

fn save_developer_mode(enabled: bool) -> LauncherResult<()> {
    let mut config = load_config()?;
    config.developer_mode = enabled;
    if !enabled && config.game_launch_action == POST_LAUNCH_OPEN_LOGS {
        config.game_launch_action = default_post_launch_action();
    }
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

fn save_applied_patch_release(tag: Option<&str>) -> LauncherResult<()> {
    let mut config = load_config()?;
    config.applied_ip_patch_release = tag.map(str::to_string);
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
    let account_username = config.connected_server_id.as_deref().and_then(|server_id| {
        config
            .accounts
            .iter()
            .find(|account| account.server_id == server_id)
            .map(|account| account.username.clone())
    });
    Ok(LauncherSnapshot {
        launcher_version: env!("OPENSHORES_LAUNCHER_VERSION").to_string(),
        installed,
        install_path: install_path.to_string_lossy().into_owned(),
        manifest,
        busy: state.active_operation.load(Ordering::SeqCst),
        game_running,
        designer_running,
        ip_patch_release: config.ip_patch_release,
        applied_ip_patch_release: config.applied_ip_patch_release,
        servers,
        connected_server_id: config.connected_server_id,
        account_username,
        developer_mode: config.developer_mode,
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

fn emit_updater(app: &AppHandle, state: &str, message: impl Into<String>) {
    let _ = app.emit(
        "updater-status",
        UpdaterStatusPayload {
            state: state.to_string(),
            message: message.into(),
        },
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
fn write_game_account_registry(host: &str, account: Option<&SavedAccount>) -> LauncherResult<bool> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = current_user
        .create_subkey(ACCOUNT_REGISTRY_PATH)
        .map_err(error_string)?;
    key.set_value("Host", &host).map_err(error_string)?;
    if let Some(account) = account {
        let password = hazeron_password_value_from_hex(&account.password_sha1)?;
        key.set_value("Name", &account.username)
            .map_err(error_string)?;
        key.set_value("Password", &password).map_err(error_string)?;
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
    let import_path = temp.join(format!("account-password-{}.reg", unique_token()));
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
fn write_game_account_registry(
    host: &str,
    account: Option<&SavedAccount>,
) -> LauncherResult<bool> {
    let key = format!("HKEY_USERS\\{}", ACCOUNT_REGISTRY_PATH);
    let wine = which(WINE_NAME).map_err(error_string)?;

    run_wine_registry_command(
        &wine,
        &["REG", "ADD", &key, "/v", "Host", "/t", "REG_SZ", "/d", host, "/f"],
    )?;
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

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

fn checked_response(response: Response, description: &str) -> LauncherResult<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(format!("{description} failed ({}).", response.status()))
    }
}

fn manifest_relative_path(value: &str) -> LauncherResult<PathBuf> {
    let mut path = PathBuf::new();
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains(['\\', ':'])
        {
            return Err(format!(
                "The game manifest contains an unsafe path: {value}"
            ));
        }
        path.push(segment);
    }
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "The game manifest contains an unsafe path: {value}"
        ));
    }
    Ok(path)
}

fn validate_game_manifest(manifest: &GameManifest) -> LauncherResult<()> {
    if manifest.files.is_empty() {
        return Err("The game manifest does not contain any files.".to_string());
    }
    manifest_base_url(manifest)?;
    let mut paths = std::collections::HashSet::new();
    let mut total_size = 0_u64;
    for entry in &manifest.files {
        manifest_relative_path(&entry.path)?;
        if !paths.insert(entry.path.to_ascii_lowercase()) {
            return Err(format!(
                "The game manifest contains a duplicate path: {}",
                entry.path
            ));
        }
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!(
                "The game manifest contains an invalid checksum for {}.",
                entry.path
            ));
        }
        total_size = total_size
            .checked_add(entry.size)
            .ok_or_else(|| "The game manifest size is invalid.".to_string())?;
    }
    if total_size != manifest.total_size {
        return Err("The game manifest total size does not match its file entries.".to_string());
    }
    for required in [GAME_EXE, GAME_DLL] {
        if !manifest
            .files
            .iter()
            .any(|entry| entry.path.eq_ignore_ascii_case(required))
        {
            return Err(format!("The game manifest is missing {required}."));
        }
    }
    Ok(())
}

fn manifest_base_url(manifest: &GameManifest) -> LauncherResult<Url> {
    let manifest_url = Url::parse(GAME_MANIFEST_URL).map_err(error_string)?;
    let base_url = manifest_url
        .join(&manifest.base_url)
        .map_err(error_string)?;
    if base_url.scheme() != "https" || base_url.host_str() != manifest_url.host_str() {
        return Err("The game manifest contains an untrusted base URL.".to_string());
    }
    Ok(base_url)
}

fn fetch_game_manifest(client: &Client) -> LauncherResult<GameManifest> {
    let response = checked_response(
        client
            .get(GAME_MANIFEST_URL)
            .timeout(METADATA_REQUEST_TIMEOUT)
            .send()
            .map_err(error_string)?,
        "Game manifest download",
    )?;
    let manifest: GameManifest = response.json().map_err(error_string)?;
    validate_game_manifest(&manifest)?;
    Ok(manifest)
}

fn manifest_file_url(manifest: &GameManifest, entry: &GameManifestFile) -> LauncherResult<Url> {
    let mut url = manifest_base_url(manifest)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "The game manifest base URL is invalid.".to_string())?;
        segments.pop_if_empty();
        for segment in entry.path.split('/') {
            segments.push(segment);
        }
    }
    Ok(url)
}

fn download_manifest_file(
    app: &AppHandle,
    client: &Client,
    manifest: &GameManifest,
    entry: &GameManifestFile,
    destination: &Path,
    completed_bytes: u64,
    index: usize,
) -> LauncherResult<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(error_string)?;
    }
    let url = manifest_file_url(manifest, entry)?;
    let mut response = checked_response(
        client.get(url).send().map_err(error_string)?,
        &format!("Game file download ({})", entry.path),
    )?;
    let mut output = File::create(destination).map_err(error_string)?;
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut received = 0_u64;
    loop {
        let count = response.read(&mut buffer).map_err(error_string)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).map_err(error_string)?;
        received += count as u64;
        let overall = completed_bytes.saturating_add(received);
        let ratio = if manifest.total_size > 0 {
            overall as f64 / manifest.total_size as f64
        } else {
            0.0
        };
        let previous_overall = overall.saturating_sub(count as u64);
        let crossed_progress_step = overall / (1024 * 1024) != previous_overall / (1024 * 1024);
        let final_file_complete = index == manifest.files.len() && received == entry.size;
        if crossed_progress_step || final_file_complete {
            emit_progress(
                app,
                "Updating clean game files",
                2.0 + ratio * 66.0,
                format!(
                    "Downloading file {index} of {} ({})",
                    manifest.files.len(),
                    format_bytes(overall)
                ),
            );
        }
    }
    output.flush().map_err(error_string)?;
    if received != entry.size {
        return Err(format!(
            "{} downloaded with the wrong size (expected {}, received {}).",
            entry.path, entry.size, received
        ));
    }
    let actual = sha256_file(destination)?;
    if !actual.eq_ignore_ascii_case(&entry.sha256) {
        return Err(format!("Checksum verification failed for {}.", entry.path));
    }
    Ok(())
}

fn stage_game_from_manifest(
    app: &AppHandle,
    client: &Client,
    manifest: &GameManifest,
    existing_install: &Path,
    game_root: &Path,
) -> LauncherResult<(usize, usize)> {
    let existing_clean_root = existing_install.join(PATCH_ORIGINALS_DIR);
    let staged_clean_root = game_root.join(PATCH_ORIGINALS_DIR);
    let mut completed_bytes = 0_u64;
    let mut reused = 0;
    let mut downloaded = 0;
    for (offset, entry) in manifest.files.iter().enumerate() {
        let relative = manifest_relative_path(&entry.path)?;
        let existing_clean = existing_clean_root.join(&relative);
        let staged_clean = staged_clean_root.join(&relative);
        let valid_clean = if existing_clean.is_file()
            && fs::metadata(&existing_clean).map_err(error_string)?.len() == entry.size
        {
            sha256_file(&existing_clean)?.eq_ignore_ascii_case(&entry.sha256)
        } else {
            false
        };
        if let Some(parent) = staged_clean.parent() {
            fs::create_dir_all(parent).map_err(error_string)?;
        }
        if valid_clean {
            fs::copy(&existing_clean, &staged_clean).map_err(error_string)?;
            reused += 1;
        } else {
            download_manifest_file(
                app,
                client,
                manifest,
                entry,
                &staged_clean,
                completed_bytes,
                offset + 1,
            )?;
            downloaded += 1;
        }
        let working = game_root.join(&relative);
        if let Some(parent) = working.parent() {
            fs::create_dir_all(parent).map_err(error_string)?;
        }
        fs::copy(&staged_clean, &working).map_err(error_string)?;
        completed_bytes = completed_bytes.saturating_add(entry.size);
        if valid_clean && ((offset + 1) % 10 == 0 || offset + 1 == manifest.files.len()) {
            emit_progress(
                app,
                "Updating clean game files",
                2.0 + completed_bytes as f64 / manifest.total_size as f64 * 66.0,
                format!(
                    "Verified {} of {} clean files",
                    offset + 1,
                    manifest.files.len()
                ),
            );
        }
    }
    Ok((reused, downloaded))
}

fn download(
    app: &AppHandle,
    client: &Client,
    url: &str,
    destination: &Path,
    phase: &str,
    range_start: f64,
    range_end: f64,
) -> LauncherResult<()> {
    let mut response = checked_response(client.get(url).send().map_err(error_string)?, "Download")?;
    let total = response.content_length().unwrap_or(0);
    let mut output = File::create(destination).map_err(error_string)?;
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut received = 0_u64;
    loop {
        let count = response.read(&mut buffer).map_err(error_string)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).map_err(error_string)?;
        received += count as u64;
        let ratio = if total > 0 {
            received as f64 / total as f64
        } else {
            0.0
        };
        let detail = if total > 0 {
            format!("{} of {}", format_bytes(received), format_bytes(total))
        } else {
            format_bytes(received)
        };
        emit_progress(
            app,
            phase,
            range_start + ratio * (range_end - range_start),
            detail,
        );
    }
    output.flush().map_err(error_string)
}

fn sha256_file(path: &Path) -> LauncherResult<String> {
    let mut input = File::open(path).map_err(error_string)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(error_string)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn verify_asset_digest(path: &Path, asset: &GithubAsset) -> LauncherResult<()> {
    if let Some(expected) = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
    {
        let actual = sha256_file(path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("Checksum verification failed for {}.", asset.name));
        }
    }
    Ok(())
}

fn verify_launcher_digest(path: &Path, asset: &GithubAsset) -> LauncherResult<()> {
    if asset.digest.is_none() {
        return Err("GitHub did not provide a checksum for the launcher update.".to_string());
    }
    verify_asset_digest(path, asset)
}

fn extract_zip(zip_path: &Path, destination: &Path) -> LauncherResult<()> {
    let input = File::open(zip_path).map_err(error_string)?;
    let mut archive = zip::ZipArchive::new(input).map_err(error_string)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(error_string)?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| "The ZIP archive contains an unsafe path.".to_string())?;
        let output_path = destination.join(enclosed);
        if entry.is_dir() {
            fs::create_dir_all(&output_path).map_err(error_string)?;
        } else {
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent).map_err(error_string)?;
            }
            let mut output = File::create(&output_path).map_err(error_string)?;
            std::io::copy(&mut entry, &mut output).map_err(error_string)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_xdelta() -> LauncherResult<PathBuf> {
    let path = launcher_data_path()?
        .join("app")
        .join("resources")
        .join(XDELTA_NAME);
    let must_write = match fs::metadata(&path) {
        Ok(metadata) => metadata.len() != XDELTA_BYTES.len() as u64,
        Err(_) => true,
    };
    if must_write {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(error_string)?;
        }
        fs::write(&path, XDELTA_BYTES).map_err(error_string)?;
    }
    Ok(path)
}

#[cfg(target_os = "linux")]
fn ensure_xdelta() -> LauncherResult<PathBuf> {
    which(XDELTA_NAME)
        .map_err(error_string)
}

fn apply_delta(source: &Path, delta: &Path, output: &Path) -> LauncherResult<()> {
    let executable = ensure_xdelta()?;
    let mut command = Command::new(executable);
    command
        .args(["-d", "-f", "-s"])
        .arg(source)
        .arg(delta)
        .arg(output);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(error_string)?;
    let started = std::time::Instant::now();
    loop {
        if child.try_wait().map_err(error_string)?.is_some() {
            break;
        }
        if started.elapsed() >= XDELTA_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Applying {} timed out after {} seconds.",
                delta.file_name().unwrap_or_default().to_string_lossy(),
                XDELTA_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    let result = child.wait_with_output().map_err(error_string)?;
    if result.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&result.stderr).trim().to_string();
        Err(format!(
            "The IP patch did not match {}. {}",
            source.file_name().unwrap_or_default().to_string_lossy(),
            if detail.is_empty() {
                format!("xdelta exited with {}", result.status)
            } else {
                detail
            }
        ))
    }
}

fn fetch_patch_releases(client: &Client) -> LauncherResult<Vec<GithubRelease>> {
    let response = checked_response(
        client
            .get(PATCH_RELEASE_API)
            .timeout(METADATA_REQUEST_TIMEOUT)
            .send()
            .map_err(error_string)?,
        "IP patch release check",
    )?;
    let mut releases: Vec<GithubRelease> = response.json().map_err(error_string)?;
    releases.retain(|release| !release.draft);
    releases.sort_by(|left, right| right.published_at.cmp(&left.published_at));
    if releases.is_empty() {
        return Err("No IP patch releases are available.".to_string());
    }
    Ok(releases)
}

fn patch_release_options(releases: &[GithubRelease]) -> Vec<PatchReleaseOption> {
    releases
        .iter()
        .map(|release| PatchReleaseOption {
            tag: release.tag_name.clone(),
            name: release
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| release.tag_name.clone()),
            published_at: release.published_at.clone(),
            has_zip: patch_zip_asset(release).is_some(),
            prerelease: release.prerelease,
        })
        .collect()
}

fn resolve_patch_release(
    releases: &[GithubRelease],
    selection: &str,
) -> LauncherResult<GithubRelease> {
    if selection.eq_ignore_ascii_case(LATEST_PATCH_RELEASE) {
        return releases
            .iter()
            .find(|release| !release.prerelease)
            .cloned()
            .ok_or_else(|| "No stable IP patch releases are available.".to_string());
    }
    releases
        .iter()
        .find(|release| release.tag_name == selection)
        .cloned()
        .ok_or_else(|| format!("The selected IP patch release ({selection}) no longer exists."))
}

fn patch_zip_asset(release: &GithubRelease) -> Option<&GithubAsset> {
    let expected = format!("{}.zip", release.tag_name);
    release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(&expected))
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|asset| asset.name.to_ascii_lowercase().ends_with(".zip"))
        })
}

fn copy_tree_overwrite(source: &Path, destination: &Path) -> LauncherResult<()> {
    fs::create_dir_all(destination).map_err(error_string)?;
    for entry in fs::read_dir(source).map_err(error_string)? {
        let entry = entry.map_err(error_string)?;
        let target = destination.join(entry.file_name());
        if entry.file_type().map_err(error_string)?.is_dir() {
            copy_tree_overwrite(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target).map_err(error_string)?;
        }
    }
    Ok(())
}

fn collect_files_with_extension(
    directory: &Path,
    extension: &str,
    files: &mut Vec<PathBuf>,
) -> LauncherResult<()> {
    for entry in fs::read_dir(directory).map_err(error_string)? {
        let entry = entry.map_err(error_string)?;
        let path = entry.path();
        if entry.file_type().map_err(error_string)?.is_dir() {
            if !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(PATCH_ORIGINALS_DIR)
            {
                collect_files_with_extension(&path, extension, files)?;
            }
        } else if path
            .extension()
            .is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case(extension))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn normalized_xdelta_source_name(value: &str) -> Option<String> {
    let filename = value.split('#').next()?.trim();
    let path = Path::new(filename);
    let extension = path.extension()?.to_string_lossy();
    let mut stem = path.file_stem()?.to_string_lossy().trim().to_string();
    for suffix in [" old", " new", " patched"] {
        if stem.to_ascii_lowercase().ends_with(suffix) {
            stem.truncate(stem.len() - suffix.len());
            break;
        }
    }
    Some(format!("{stem}.{extension}"))
}

fn xdelta_source_name(delta: &Path) -> LauncherResult<String> {
    let executable = ensure_xdelta()?;
    let mut command = Command::new(executable);
    command.arg("printhdr").arg(delta);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command.output().map_err(error_string)?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    combined
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("XDELTA filename (source):")
                .and_then(normalized_xdelta_source_name)
        })
        .ok_or_else(|| {
            format!(
                "Could not determine the source file for {}.",
                delta.display()
            )
        })
}

fn find_file_by_name(directory: &Path, filename: &str) -> LauncherResult<Option<PathBuf>> {
    for entry in fs::read_dir(directory).map_err(error_string)? {
        let entry = entry.map_err(error_string)?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(PATCH_ORIGINALS_DIR)
        {
            continue;
        }
        let path = entry.path();
        if entry.file_type().map_err(error_string)?.is_dir() {
            if let Some(found) = find_file_by_name(&path, filename)? {
                return Ok(Some(found));
            }
        } else if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(filename)
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn apply_xdeltas(game_root: &Path) -> LauncherResult<(usize, usize)> {
    let mut deltas = Vec::new();
    collect_files_with_extension(game_root, "xdelta", &mut deltas)?;
    let originals = game_root.join(PATCH_ORIGINALS_DIR);
    let mut applied = 0;
    let mut skipped = 0;
    for delta in deltas {
        let source_name = match xdelta_source_name(&delta) {
            Ok(name) => name,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let nearby = delta.parent().unwrap_or(game_root).join(&source_name);
        let target = if nearby.is_file() {
            Some(nearby)
        } else {
            find_file_by_name(game_root, &source_name)?
        };
        let Some(target) = target else {
            skipped += 1;
            continue;
        };
        let relative = target.strip_prefix(game_root).map_err(error_string)?;
        let original = originals.join(relative);
        if !original.is_file() {
            if let Some(parent) = original.parent() {
                fs::create_dir_all(parent).map_err(error_string)?;
            }
            fs::copy(&target, &original).map_err(error_string)?;
        }
        let output = target.with_extension(format!(
            "{}.patched",
            target.extension().unwrap_or_default().to_string_lossy()
        ));
        if let Err(error) = apply_delta(&original, &delta, &output) {
            let _ = fs::remove_file(&output);
            return Err(error);
        }
        fs::copy(&output, &target).map_err(error_string)?;
        fs::remove_file(&output).map_err(error_string)?;
        fs::remove_file(&delta).map_err(error_string)?;
        applied += 1;
    }
    Ok((applied, skipped))
}

fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos:x}", std::process::id())
}

fn single_directory_or_self(path: &Path) -> LauncherResult<PathBuf> {
    let entries: Vec<_> = fs::read_dir(path)
        .map_err(error_string)?
        .filter_map(Result::ok)
        .collect();
    if entries.len() == 1 && entries[0].file_type().map_err(error_string)?.is_dir() {
        Ok(entries[0].path())
    } else {
        Ok(path.to_path_buf())
    }
}

fn download_and_apply_patch(
    app: &AppHandle,
    client: &Client,
    release: &GithubRelease,
    game_root: &Path,
    work: &Path,
    range_start: f64,
    range_end: f64,
) -> LauncherResult<()> {
    let asset = patch_zip_asset(release).ok_or_else(|| {
        format!(
            "IP patch release {} does not contain a ZIP asset.",
            release.tag_name
        )
    })?;
    fs::create_dir_all(work).map_err(error_string)?;
    let zip_path = work.join(&asset.name);
    let extract_path = work.join("extracted");
    fs::create_dir_all(&extract_path).map_err(error_string)?;
    download(
        app,
        client,
        &asset.browser_download_url,
        &zip_path,
        "Downloading the IP patch",
        range_start,
        range_end,
    )?;
    verify_asset_digest(&zip_path, asset)?;
    emit_progress(
        app,
        "Unpacking the IP patch",
        range_end,
        format!("Installing files from {}...", asset.name),
    );
    extract_zip(&zip_path, &extract_path)?;
    let patch_root = single_directory_or_self(&extract_path)?;
    copy_tree_overwrite(&patch_root, game_root)?;
    emit_progress(
        app,
        "Applying the IP patch",
        (range_end + 96.0) / 2.0,
        format!("Applying {}...", release.tag_name),
    );
    let (applied, skipped) = apply_xdeltas(game_root)?;
    let detail = if skipped == 0 {
        format!("Applied {applied} xdelta patch(es).")
    } else {
        format!("Applied {applied} xdelta patch(es); left {skipped} unmatched patch(es).")
    };
    emit_progress(app, "IP patch installed", 96.0, detail);
    Ok(())
}

fn update_manifest_patch(game_root: &Path, release: &GithubRelease) -> LauncherResult<()> {
    let path = game_root.join(MANIFEST_FILE);
    let mut manifest = read_json::<Value>(&path).unwrap_or_else(|| json!({}));
    let object = manifest
        .as_object_mut()
        .ok_or_else(|| "The launcher manifest is invalid.".to_string())?;
    object.insert("patchTag".to_string(), json!(release.tag_name));
    object.insert("patchPublishedAt".to_string(), json!(release.published_at));
    write_json(&path, &manifest)
}

fn update_ip_patch_sync(
    app: &AppHandle,
    state: &AppState,
    force: bool,
) -> LauncherResult<LauncherSnapshot> {
    if any_game_process_running(state)? {
        return Err("Close OpenShores before updating the IP patch.".to_string());
    }
    let config = load_config()?;
    let install_path = PathBuf::from(config.install_path.clone().unwrap());
    if !is_managed_manifest(read_json::<Value>(&install_path.join(MANIFEST_FILE)).as_ref()) {
        return get_snapshot(state);
    }
    if config
        .ip_patch_release
        .eq_ignore_ascii_case(DISABLED_PATCH_RELEASE)
    {
        return install_game_sync(app, state, install_path);
    }
    let guard = begin_operation(state)?;
    if any_game_process_running(state)? {
        return Err("Close OpenShores before updating the IP patch.".to_string());
    }
    emit_operation_status(app, true, None);
    emit_progress(
        app,
        "Checking the IP patch",
        1.0,
        "Contacting GitHub for the selected release...",
    );
    let mut work = None;
    let result = (|| {
        let client = http_client()?;
        let releases = fetch_patch_releases(&client)?;
        let release = resolve_patch_release(&releases, &config.ip_patch_release)?;
        if !force && config.applied_ip_patch_release.as_deref() == Some(&release.tag_name) {
            emit_progress(
                app,
                "IP patch is up to date",
                100.0,
                format!("{} is already installed.", release.tag_name),
            );
            return get_snapshot(state);
        }
        emit_progress(
            app,
            "Checking the IP patch",
            3.0,
            format!("Selected release: {}", release.tag_name),
        );
        let patch_work = launcher_temp_path()?.join(format!("ip-patch-{}", unique_token()));
        work = Some(patch_work.clone());
        download_and_apply_patch(
            app,
            &client,
            &release,
            &install_path,
            &patch_work,
            5.0,
            75.0,
        )?;
        update_manifest_patch(&install_path, &release)?;
        save_applied_patch_release(Some(&release.tag_name))?;
        emit_progress(
            app,
            "IP patch is up to date",
            100.0,
            format!("{} is installed.", release.tag_name),
        );
        get_snapshot(state)
    })();
    if let Some(work) = work {
        let _ = fs::remove_dir_all(work);
    }
    drop(guard);
    match result {
        Ok(mut snapshot) => {
            snapshot.busy = false;
            emit_operation_status(app, false, None);
            let _ = app.emit("state-changed", snapshot.clone());
            Ok(snapshot)
        }
        Err(error) => {
            emit_operation_status(app, false, Some(error.clone()));
            Err(error)
        }
    }
}

fn install_game_sync(
    app: &AppHandle,
    state: &AppState,
    install_path: PathBuf,
) -> LauncherResult<LauncherSnapshot> {
    let _guard = begin_operation(state)?;
    if install_path.exists() {
        let has_contents = fs::read_dir(&install_path)
            .map_err(error_string)?
            .next()
            .is_some();
        let manifest = read_json::<Value>(&install_path.join(MANIFEST_FILE));
        if has_contents && !is_managed_manifest(manifest.as_ref()) {
            return Err("The selected folder is not empty and is not managed by this launcher. Choose an empty folder to keep existing files safe.".to_string());
        }
    }

    let parent = install_path
        .parent()
        .ok_or_else(|| "The installation folder has no parent directory.".to_string())?;
    fs::create_dir_all(parent).map_err(error_string)?;
    let token = unique_token();
    let work = parent.join(format!(".openshores-staging-{token}"));
    let game_root = work.join("game");
    let patch_path = work.join("patch");
    let backup_path = parent.join(format!(".openshores-backup-{token}"));
    let mut backup_active = false;

    let result = (|| {
        fs::create_dir_all(&game_root).map_err(error_string)?;
        let client = http_client()?;

        emit_progress(
            app,
            "Getting the game manifest",
            1.0,
            "Connecting to openshores.net...",
        );
        let game_manifest = fetch_game_manifest(&client)?;
        let (reused, downloaded) =
            stage_game_from_manifest(app, &client, &game_manifest, &install_path, &game_root)?;
        emit_progress(
            app,
            "Clean game files ready",
            69.0,
            format!("Reused {reused} files and downloaded {downloaded} files."),
        );
        let exe_path = game_root.join(GAME_EXE);
        let dll_path = game_root.join(GAME_DLL);
        if !exe_path.is_file() || !dll_path.is_file() {
            return Err("The OpenShores manifest is missing required game files.".to_string());
        }

        let selection = load_config()?.ip_patch_release;
        let release = if selection.eq_ignore_ascii_case(DISABLED_PATCH_RELEASE) {
            emit_progress(
                app,
                "IP patch disabled",
                84.0,
                "Installing clean game files without the IP patch.",
            );
            None
        } else {
            emit_progress(
                app,
                "Getting the IP patch",
                71.0,
                "Checking the selected patch release...",
            );
            let releases = fetch_patch_releases(&client)?;
            let release = resolve_patch_release(&releases, &selection)?;
            download_and_apply_patch(app, &client, &release, &game_root, &patch_path, 72.0, 84.0)?;
            Some(release)
        };

        let installed_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(error_string)?;
        let mut launcher_manifest = json!({
            "managedBy": MANAGED_BY,
            "launcherVersion": env!("OPENSHORES_LAUNCHER_VERSION"),
            "installedAt": installed_at,
            "gameManifestUrl": GAME_MANIFEST_URL,
            "gameManifestVersion": game_manifest.version,
            "gameManifestGenerated": game_manifest.generated,
            "gameManifestFileCount": game_manifest.files.len()
        });
        if let Some(release) = release.as_ref() {
            let object = launcher_manifest.as_object_mut().unwrap();
            object.insert("patchTag".to_string(), json!(release.tag_name));
            object.insert("patchPublishedAt".to_string(), json!(release.published_at));
        }
        write_json(&game_root.join(MANIFEST_FILE), &launcher_manifest)?;
        emit_progress(
            app,
            "Finishing installation",
            95.0,
            "Activating the new installation...",
        );
        if install_path.exists() {
            fs::rename(&install_path, &backup_path).map_err(error_string)?;
            backup_active = true;
        }
        fs::rename(&game_root, &install_path).map_err(error_string)?;
        if backup_active {
            fs::remove_dir_all(&backup_path).map_err(error_string)?;
            backup_active = false;
        }
        save_install_path(&install_path)?;
        save_applied_patch_release(release.as_ref().map(|release| release.tag_name.as_str()))?;
        emit_progress(
            app,
            "Ready to play",
            100.0,
            if release.is_some() {
                "OpenShores and the IP patch are installed."
            } else {
                "OpenShores is installed without the IP patch."
            },
        );
        get_snapshot(state)
    })();

    if result.is_err() && backup_active && backup_path.exists() && !install_path.exists() {
        let _ = fs::rename(&backup_path, &install_path);
    }
    let _ = fs::remove_dir_all(&work);
    result
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
            command.arg("-launcher");
            if let Some(server_id) = config.connected_server_id.as_deref() {
                let server = server_by_id(&config, server_id)?;
                let active_account = config
                    .accounts
                    .iter()
                    .find(|account| account.server_id == server.id);
                if write_game_account_registry(&server.host, active_account)? {
                    command.arg("-nologin");
                }
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

fn parse_release_version(release: &GithubRelease) -> LauncherResult<Version> {
    let value = if release.tag_name.trim().is_empty() {
        release.name.as_deref().unwrap_or_default()
    } else {
        &release.tag_name
    };
    Version::parse(value.trim().trim_start_matches(['v', 'V']))
        .map_err(|_| "The latest launcher release has no valid semantic version tag.".to_string())
}

fn download_launcher_update(
    app: &AppHandle,
    client: &Client,
    asset: &GithubAsset,
    version: &Version,
) -> LauncherResult<PathBuf> {
    let temp = launcher_temp_path()?;
    fs::create_dir_all(&temp).map_err(error_string)?;
    let destination = temp.join(format!("OpenShores-Launcher-{version}.download.exe"));
    emit_updater(
        app,
        "downloading",
        format!("Downloading launcher {version} - 0%"),
    );
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
            emit_updater(
                app,
                "downloading",
                format!("Downloading launcher {version} - {percent}%"),
            );
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

fn check_updates_sync(
    app: &AppHandle,
    state: &AppState,
    manual: bool,
) -> LauncherResult<UpdaterStatusPayload> {
    let client = http_client()?;
    let response = client
        .get(LAUNCHER_RELEASE_API)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .map_err(error_string)?;
    if response.status().as_u16() == 404 {
        return Ok(UpdaterStatusPayload {
            state: "current".to_string(),
            message: "The launcher update channel has not been published yet.".to_string(),
        });
    }
    let response = checked_response(response, "Launcher update check")?;
    let release: GithubRelease = response.json().map_err(error_string)?;
    let release_version = parse_release_version(&release)?;
    let current_version =
        Version::parse(env!("OPENSHORES_LAUNCHER_VERSION")).map_err(error_string)?;
    if release_version <= current_version {
        *state.pending_update.lock().map_err(error_string)? = None;
        return Ok(UpdaterStatusPayload {
            state: "current".to_string(),
            message: "Launcher is up to date.".to_string(),
        });
    }
    let binary_name = if cfg!(target_os = "linux") { "OpenShores-Launcher-amd64.AppImage" } else { "OpenShores-Launcher.exe" };
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(binary_name))
        .ok_or_else(|| {
            format!("Launcher {release_version} does not include a portable Windows executable.")
        })?;
    *state.pending_update.lock().map_err(error_string)? = Some(PendingUpdate {
        asset: asset.clone(),
        version: release_version.clone(),
    });
    let status = UpdaterStatusPayload {
        state: "available".to_string(),
        message: format!("Launcher {release_version} is available."),
    };
    if !manual {
        let _ = app.emit("updater-status", status.clone());
    }
    Ok(status)
}

fn check_ip_patch_update_sync() -> LauncherResult<UpdaterStatusPayload> {
    let config = load_config()?;
    let install_path = PathBuf::from(config.install_path.clone().unwrap());
    if !is_managed_manifest(read_json::<Value>(&install_path.join(MANIFEST_FILE)).as_ref()) {
        return Ok(UpdaterStatusPayload {
            state: "current".to_string(),
            message: "Install OpenShores before checking the IP patch.".to_string(),
        });
    }

    if config
        .ip_patch_release
        .eq_ignore_ascii_case(DISABLED_PATCH_RELEASE)
    {
        return Ok(if config.applied_ip_patch_release.is_some() {
            UpdaterStatusPayload {
                state: "available".to_string(),
                message: "The installed IP patch can be removed.".to_string(),
            }
        } else {
            UpdaterStatusPayload {
                state: "current".to_string(),
                message: "The IP patch is disabled.".to_string(),
            }
        });
    }

    let client = http_client()?;
    let releases = fetch_patch_releases(&client)?;
    let release = resolve_patch_release(&releases, &config.ip_patch_release)?;
    if config.applied_ip_patch_release.as_deref() == Some(&release.tag_name) {
        return Ok(UpdaterStatusPayload {
            state: "current".to_string(),
            message: format!("IP patch {} is up to date.", release.tag_name),
        });
    }

    Ok(UpdaterStatusPayload {
        state: "available".to_string(),
        message: format!("IP patch {} is available.", release.tag_name),
    })
}

fn game_manifest_update_available(local: Option<&Value>, remote: &GameManifest) -> bool {
    let Some(local) = local else {
        return false;
    };
    local.get("gameManifestVersion").and_then(Value::as_str) != Some(remote.version.as_str())
        || local.get("gameManifestGenerated").and_then(Value::as_u64) != Some(remote.generated)
        || local.get("gameManifestFileCount").and_then(Value::as_u64)
            != Some(remote.files.len() as u64)
}

fn check_game_update_sync() -> LauncherResult<UpdaterStatusPayload> {
    let config = load_config()?;
    let install_path = PathBuf::from(config.install_path.clone().unwrap());
    let local = read_json::<Value>(&install_path.join(MANIFEST_FILE));
    if !is_managed_manifest(local.as_ref()) || !install_path.join(GAME_EXE).is_file() {
        return Ok(UpdaterStatusPayload {
            state: "current".to_string(),
            message: "Install OpenShores before checking game files.".to_string(),
        });
    }

    let remote = fetch_game_manifest(&http_client()?)?;
    if game_manifest_update_available(local.as_ref(), &remote) {
        Ok(UpdaterStatusPayload {
            state: "available".to_string(),
            message: format!("OpenShores client {} is available.", remote.version),
        })
    } else {
        Ok(UpdaterStatusPayload {
            state: "current".to_string(),
            message: "OpenShores game files are up to date.".to_string(),
        })
    }
}

fn report_previous_update_error(app: &AppHandle) {
    let Ok(path) = launcher_data_path().map(|path| path.join("update-error.log")) else {
        return;
    };
    if let Ok(message) = fs::read_to_string(&path) {
        let _ = fs::remove_file(path);
        if !message.trim().is_empty() {
            emit_updater(app, "error", message.trim());
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

    for file in [app_archive, legacy_app.join("resources").join(XDELTA_NAME)] {
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

fn directory_file_bytes(path: &Path) -> LauncherResult<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path).map_err(error_string)? {
        let entry = entry.map_err(error_string)?;
        let file_type = entry.file_type().map_err(error_string)?;
        if file_type.is_symlink() {
            return Err(
                "The installation contains a symbolic link and cannot be moved safely.".to_string(),
            );
        }
        if file_type.is_dir() {
            total = total.saturating_add(directory_file_bytes(&entry.path())?);
        } else if file_type.is_file() {
            total = total.saturating_add(entry.metadata().map_err(error_string)?.len());
        }
    }
    Ok(total)
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

fn read_client_log_sync(offset: u64) -> LauncherResult<ClientLogChunk> {
    let config = load_config()?;
    if !config.developer_mode {
        return Err("Developer mode must be enabled to read the client log.".to_string());
    }
    let log_path = PathBuf::from(
        config
            .install_path
            .unwrap_or(default_install_path()?.to_string_lossy().into_owned()),
    )
    .join(CLIENT_LOG);
    if !log_path.is_file() {
        return Ok(ClientLogChunk {
            content: String::new(),
            next_offset: 0,
            reset: offset > 0,
        });
    }

    let mut file = File::open(log_path).map_err(error_string)?;
    let length = file.metadata().map_err(error_string)?.len();
    let reset = length < offset;
    let start = if reset { 0 } else { offset };
    file.seek(SeekFrom::Start(start)).map_err(error_string)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(error_string)?;
    Ok(ClientLogChunk {
        next_offset: start + bytes.len() as u64,
        content: String::from_utf8_lossy(&bytes).into_owned(),
        reset,
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
    let log_path = PathBuf::from(
        config
            .install_path
            .unwrap_or(default_install_path()?.to_string_lossy().into_owned()),
    )
    .join(CLIENT_LOG);
    if !log_path.is_file() {
        return Ok(());
    }
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
async fn get_ip_patch_releases() -> LauncherResult<Vec<PatchReleaseOption>> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = http_client()?;
        fetch_patch_releases(&client).map(|releases| patch_release_options(&releases))
    })
    .await
    .map_err(error_string)?
}

#[tauri::command]
async fn set_ip_patch_release(
    state: State<'_, AppState>,
    selection: String,
) -> LauncherResult<LauncherSnapshot> {
    save_patch_selection(selection)?;
    get_snapshot(state.inner())
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
async fn update_ip_patch(
    app: AppHandle,
    state: State<'_, AppState>,
) -> LauncherResult<LauncherSnapshot> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || update_ip_patch_sync(&app, &backend, false))
        .await
        .map_err(error_string)?
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
    if cmd != WINE_NAME && cmd != XDELTA_NAME {
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
    let client = account_client()?;
    let response = client
        .post(server_api_url(&server.host, endpoint)?)
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

#[tauri::command]
fn open_link(url: String) -> LauncherResult<()> {
    let allowed = [
        "https://openshores.net/",
        "https://github.com/Celarious/OpenShores-IP-Patch",
        "https://github.com/Norway174/OpenShores-Launcher",
    ];
    if !allowed.iter().any(|prefix| url.starts_with(prefix)) {
        return Err("Blocked external URL.".to_string());
    }
    open::that(url).map_err(error_string)
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
async fn check_ip_patch_update() -> LauncherResult<UpdaterStatusPayload> {
    tauri::async_runtime::spawn_blocking(check_ip_patch_update_sync)
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
    emit_updater(
        app,
        "installing",
        format!("Installing launcher {}...", pending.version),
    );

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
        emit_updater(&app, "error", format!("Launcher update failed: {error}"));
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
            read_client_log,
            clear_client_log,
            choose_folder,
            install_game,
            get_ip_patch_releases,
            set_ip_patch_release,
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
            update_ip_patch,
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
            check_updates,
            check_ip_patch_update,
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

    fn test_game_manifest() -> GameManifest {
        GameManifest {
            version: "test".to_string(),
            generated: 1,
            base_url: "/downloads/client/".to_string(),
            total_size: 30,
            files: vec![
                GameManifestFile {
                    path: GAME_EXE.to_string(),
                    size: 20,
                    sha256: "a".repeat(64),
                },
                GameManifestFile {
                    path: GAME_DLL.to_string(),
                    size: 10,
                    sha256: "b".repeat(64),
                },
            ],
        }
    }

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
    fn game_update_check_compares_manifest_identity() {
        let remote = test_game_manifest();
        let current = json!({
            "gameManifestVersion": remote.version.clone(),
            "gameManifestGenerated": remote.generated,
            "gameManifestFileCount": remote.files.len()
        });
        assert!(!game_manifest_update_available(Some(&current), &remote));

        let older = json!({
            "gameManifestVersion": "older",
            "gameManifestGenerated": remote.generated,
            "gameManifestFileCount": remote.files.len()
        });
        assert!(game_manifest_update_available(Some(&older), &remote));
    }

    #[test]
    fn release_versions_accept_a_v_prefix() {
        let release = GithubRelease {
            tag_name: "v1.2.3".to_string(),
            name: None,
            published_at: None,
            draft: false,
            prerelease: false,
            assets: Vec::new(),
        };
        assert_eq!(
            parse_release_version(&release).unwrap(),
            Version::new(1, 2, 3)
        );
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
    fn xdelta_payload_is_embedded() {
        assert!(XDELTA_BYTES.len() > 500_000);
    }

    #[test]
    fn offline_designer_modes_map_to_expected_arguments() {
        assert_eq!(designer_launch_argument("spacecraft").unwrap(), "-spacecraft");
        assert_eq!(designer_launch_argument("building").unwrap(), "-building");
        assert_eq!(designer_launch_argument("designer").unwrap(), "-designer");
        assert!(designer_launch_argument("unknown").is_err());
    }

    #[test]
    fn xdelta_source_labels_are_normalized_without_patch_name_rules() {
        assert_eq!(
            normalized_xdelta_source_name("Shores of Hazeron.exe#a742ce94d906994a38c4ecd252076b2a")
                .as_deref(),
            Some("Shores of Hazeron.exe")
        );
        assert_eq!(
            normalized_xdelta_source_name("AuLoginClient13 old.dll#714b63cc").as_deref(),
            Some("AuLoginClient13.dll")
        );
        assert_eq!(
            normalized_xdelta_source_name("AuUtil13.dll#e7b5eb27").as_deref(),
            Some("AuUtil13.dll")
        );
    }

    #[test]
    fn patch_release_selection_uses_strings_for_latest_and_pinned_versions() {
        let releases = vec![
            GithubRelease {
                tag_name: "r4".to_string(),
                name: Some("Fourth release".to_string()),
                published_at: Some("2026-08-07T02:32:45Z".to_string()),
                draft: false,
                prerelease: false,
                assets: Vec::new(),
            },
            GithubRelease {
                tag_name: "r3".to_string(),
                name: Some("Third release".to_string()),
                published_at: Some("2026-08-07T01:17:58Z".to_string()),
                draft: false,
                prerelease: false,
                assets: Vec::new(),
            },
        ];
        assert_eq!(
            resolve_patch_release(&releases, LATEST_PATCH_RELEASE)
                .unwrap()
                .tag_name,
            "r4"
        );
        assert_eq!(
            resolve_patch_release(&releases, "r3").unwrap().tag_name,
            "r3"
        );
    }

    #[test]
    fn existing_configs_default_to_latest_patch_release() {
        let config: LauncherConfig =
            serde_json::from_str(r#"{"installPath":"C:\\OpenShores"}"#).unwrap();
        assert_eq!(config.ip_patch_release, LATEST_PATCH_RELEASE);
        assert_eq!(config.applied_ip_patch_release, None);
        assert_eq!(config.servers, None);
        assert_eq!(config.game_launch_action, POST_LAUNCH_DO_NOTHING);
        assert_eq!(config.designer_launch_action, POST_LAUNCH_DO_NOTHING);

        let saved = LauncherConfig {
            applied_ip_patch_release: Some("r4".to_string()),
            ..config
        };
        let json = serde_json::to_value(saved).unwrap();
        assert_eq!(json["appliedIpPatchRelease"], "r4");
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
        };
        let json = serde_json::to_string(&account).unwrap();
        assert!(json.contains("e5e9fa1ba31ecd1ae84f75caaa474f3a663f05f4"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn multiple_servers_keep_independent_saved_accounts() {
        let mut config = LauncherConfig::default();
        config.accounts = vec![
            SavedAccount {
                server_id: "openshores".to_string(),
                username: "Explorer".to_string(),
                password_sha1: password_sha1_hex("first"),
            },
            SavedAccount {
                server_id: "community".to_string(),
                username: "Builder".to_string(),
                password_sha1: password_sha1_hex("second"),
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

    #[test]
    fn game_manifest_paths_are_safe_and_urls_encode_filenames() {
        let manifest = test_game_manifest();
        validate_game_manifest(&manifest).unwrap();
        assert!(manifest_relative_path("../outside.dll").is_err());
        assert!(manifest_relative_path(r"folder\outside.dll").is_err());
        assert_eq!(
            manifest_file_url(&manifest, &manifest.files[0])
                .unwrap()
                .as_str(),
            "https://openshores.net/downloads/client/Shores%20of%20Hazeron.exe"
        );
    }

    #[test]
    fn game_manifest_rejects_duplicate_paths_and_wrong_totals() {
        let mut duplicate = test_game_manifest();
        duplicate.files[1].path = GAME_EXE.to_ascii_lowercase();
        assert!(validate_game_manifest(&duplicate)
            .unwrap_err()
            .contains("duplicate"));

        let mut wrong_total = test_game_manifest();
        wrong_total.total_size += 1;
        assert!(validate_game_manifest(&wrong_total)
            .unwrap_err()
            .contains("total size"));
    }
}
