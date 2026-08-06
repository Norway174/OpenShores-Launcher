#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use reqwest::blocking::{Client, Response};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use semver::Version;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
fn hide_native_window_border(window: &WebviewWindow) {
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_COLOR_NONE: u32 = 0xffff_fffe;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut std::ffi::c_void,
            attribute: u32,
            value: *const std::ffi::c_void,
            value_size: u32,
        ) -> i32;
    }

    if let Ok(hwnd) = window.hwnd() {
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd.0,
                DWMWA_BORDER_COLOR,
                (&DWMWA_COLOR_NONE as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

const GAME_URL: &str = "https://openshores.net/downloads/OpenShores.zip";
const PATCH_RELEASE_API: &str =
    "https://api.github.com/repos/Celarious/OpenShores-IP-Patch/releases/latest";
const LAUNCHER_RELEASE_API: &str =
    "https://api.github.com/repos/Norway174/OpenShores-Launcher/releases/latest";
const GAME_EXE: &str = "Shores of Hazeron.exe";
const GAME_DLL: &str = "AuLoginClient13.dll";
const MANIFEST_FILE: &str = ".openshores-launcher.json";
const MANAGED_BY: &str = "OpenShores Launcher";
const XDELTA_NAME: &str = "xdelta3-3.0.11-x86_64.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const XDELTA_BYTES: &[u8] = include_bytes!("../../resources/xdelta3/xdelta3-3.0.11-x86_64.exe");

type LauncherResult<T> = Result<T, String>;

#[derive(Clone, Default)]
struct AppState {
    active_operation: Arc<AtomicBool>,
    game_pid: Arc<Mutex<Option<u32>>>,
    pending_update: Arc<Mutex<Option<PendingUpdate>>>,
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct LauncherConfig {
    #[serde(rename = "installPath", skip_serializing_if = "Option::is_none")]
    install_path: Option<String>,
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
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    phase: String,
    percent: u8,
    detail: String,
}

#[derive(Clone, Serialize)]
struct GameStatusPayload {
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct UpdaterStatusPayload {
    state: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    published_at: Option<String>,
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

fn local_app_data() -> LauncherResult<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "Windows Local AppData could not be located.".to_string())
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

    if config.install_path.is_none() {
        if let Some(roaming) = env::var_os("APPDATA").map(PathBuf::from) {
            for previous in [
                roaming.join("OpenShores Launcher").join("config.json"),
                roaming.join("openshores-launcher").join("config.json"),
            ] {
                if let Some(found) = read_json::<LauncherConfig>(&previous) {
                    if found.install_path.is_some() {
                        config = found;
                        write_json(&path, &config)?;
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
            }
        }
    }

    if config.install_path.is_none() {
        config.install_path = Some(default_install_path()?.to_string_lossy().into_owned());
    }
    Ok(config)
}

fn save_install_path(install_path: &Path) -> LauncherResult<()> {
    let config = LauncherConfig {
        install_path: Some(install_path.to_string_lossy().into_owned()),
    };
    write_json(&config_path()?, &config)
}

fn get_snapshot(state: &AppState) -> LauncherResult<LauncherSnapshot> {
    let config = load_config()?;
    let install_path = PathBuf::from(
        config
            .install_path
            .unwrap_or(default_install_path()?.to_string_lossy().into_owned()),
    );
    let manifest = read_json::<Value>(&install_path.join(MANIFEST_FILE));
    let installed = manifest.is_some() && install_path.join(GAME_EXE).is_file();
    let game_running = state.game_pid.lock().map_err(error_string)?.is_some();
    Ok(LauncherSnapshot {
        launcher_version: env!("CARGO_PKG_VERSION").to_string(),
        installed,
        install_path: install_path.to_string_lossy().into_owned(),
        manifest,
        busy: state.active_operation.load(Ordering::SeqCst),
        game_running,
    })
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
        .user_agent(format!("OpenShores-Launcher/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(error_string)
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
    let result = command.output().map_err(error_string)?;
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

fn fetch_patch_release(client: &Client) -> LauncherResult<GithubRelease> {
    let response = checked_response(
        client.get(PATCH_RELEASE_API).send().map_err(error_string)?,
        "IP patch release check",
    )?;
    let release: GithubRelease = response.json().map_err(error_string)?;
    for required in [
        "SoH_delta.xdelta",
        "auloginclient_delta.xdelta",
        "Redirect.dll",
    ] {
        if !release
            .assets
            .iter()
            .any(|asset| asset.name.eq_ignore_ascii_case(required))
        {
            return Err(format!(
                "Patch release {} is missing {required}.",
                release.tag_name
            ));
        }
    }
    Ok(release)
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
    let zip_path = work.join("OpenShores.zip");
    let extract_path = work.join("extracted");
    let patch_path = work.join("patch");
    let backup_path = parent.join(format!(".openshores-backup-{token}"));
    let mut backup_active = false;

    let result = (|| {
        fs::create_dir_all(&extract_path).map_err(error_string)?;
        fs::create_dir_all(&patch_path).map_err(error_string)?;
        let client = http_client()?;

        emit_progress(
            app,
            "Downloading OpenShores",
            1.0,
            "Connecting to openshores.net...",
        );
        download(
            app,
            &client,
            GAME_URL,
            &zip_path,
            "Downloading OpenShores",
            2.0,
            61.0,
        )?;
        emit_progress(
            app,
            "Unpacking game files",
            64.0,
            "Extracting the official client...",
        );
        extract_zip(&zip_path, &extract_path)?;
        let game_root = single_directory_or_self(&extract_path)?;
        let exe_path = game_root.join(GAME_EXE);
        let dll_path = game_root.join(GAME_DLL);
        if !exe_path.is_file() || !dll_path.is_file() {
            return Err("The OpenShores archive is missing required game files.".to_string());
        }

        emit_progress(
            app,
            "Getting the IP patch",
            70.0,
            "Checking the latest patch release...",
        );
        let release = fetch_patch_release(&client)?;
        let patch_names = [
            "SoH_delta.xdelta",
            "auloginclient_delta.xdelta",
            "Redirect.dll",
        ];
        for (index, name) in patch_names.iter().enumerate() {
            let asset = release
                .assets
                .iter()
                .find(|asset| asset.name.eq_ignore_ascii_case(name))
                .unwrap();
            let target = patch_path.join(name);
            download(
                app,
                &client,
                &asset.browser_download_url,
                &target,
                "Getting the IP patch",
                71.0 + index as f64 * 4.0,
                74.0 + index as f64 * 4.0,
            )?;
            verify_asset_digest(&target, asset)?;
        }

        emit_progress(
            app,
            "Applying the IP patch",
            85.0,
            format!("Applying {}...", release.tag_name),
        );
        let patched_exe = exe_path.with_extension("exe.patched");
        let patched_dll = dll_path.with_extension("dll.patched");
        apply_delta(
            &exe_path,
            &patch_path.join("SoH_delta.xdelta"),
            &patched_exe,
        )?;
        apply_delta(
            &dll_path,
            &patch_path.join("auloginclient_delta.xdelta"),
            &patched_dll,
        )?;
        fs::copy(&patched_exe, &exe_path).map_err(error_string)?;
        fs::copy(&patched_dll, &dll_path).map_err(error_string)?;
        let _ = fs::remove_file(&patched_exe);
        let _ = fs::remove_file(&patched_dll);
        fs::copy(
            patch_path.join("Redirect.dll"),
            game_root.join("Redirect.dll"),
        )
        .map_err(error_string)?;

        let archive_hash = sha256_file(&zip_path)?;
        let installed_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(error_string)?;
        write_json(
            &game_root.join(MANIFEST_FILE),
            &json!({
                "managedBy": MANAGED_BY,
                "launcherVersion": env!("CARGO_PKG_VERSION"),
                "installedAt": installed_at,
                "gameUrl": GAME_URL,
                "gameArchiveSha256": archive_hash,
                "patchTag": release.tag_name,
                "patchPublishedAt": release.published_at
            }),
        )?;

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
        emit_progress(
            app,
            "Ready to play",
            100.0,
            "OpenShores and the IP patch are installed.",
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
    if state.game_pid.lock().map_err(error_string)?.is_some() {
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

fn launch_game_sync(app: &AppHandle, state: &AppState) -> LauncherResult<LauncherSnapshot> {
    let config = load_config()?;
    let install_path = PathBuf::from(config.install_path.unwrap());
    let executable = install_path.join(GAME_EXE);
    if !executable.is_file() {
        return Err("OpenShores is not installed.".to_string());
    }
    {
        let mut pid = state.game_pid.lock().map_err(error_string)?;
        if pid.is_some() {
            return get_snapshot(state);
        }
        let mut child = Command::new(&executable)
            .current_dir(&install_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(error_string)?;
        *pid = Some(child.id());
        let game_pid = state.game_pid.clone();
        let app_handle = app.clone();
        thread::spawn(move || {
            let result = child.wait();
            if let Ok(mut current) = game_pid.lock() {
                *current = None;
            }
            let error = result.err().map(|value| value.to_string());
            let _ = app_handle.emit(
                "game-status",
                GameStatusPayload {
                    running: false,
                    error,
                },
            );
        });
    }
    let _ = app.emit(
        "game-status",
        GameStatusPayload {
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

fn check_updates_sync(app: &AppHandle, state: &AppState, manual: bool) -> LauncherResult<()> {
    let client = http_client()?;
    let response = client
        .get(LAUNCHER_RELEASE_API)
        .send()
        .map_err(error_string)?;
    if response.status().as_u16() == 404 {
        if manual {
            emit_updater(
                app,
                "current",
                "The launcher update channel has not been published yet.",
            );
        }
        return Ok(());
    }
    let response = checked_response(response, "Launcher update check")?;
    let release: GithubRelease = response.json().map_err(error_string)?;
    let release_version = parse_release_version(&release)?;
    let current_version = Version::parse(env!("CARGO_PKG_VERSION")).map_err(error_string)?;
    if release_version <= current_version {
        *state.pending_update.lock().map_err(error_string)? = None;
        if manual {
            emit_updater(app, "current", "Launcher is up to date.");
        }
        return Ok(());
    }
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case("OpenShores-Launcher.exe"))
        .ok_or_else(|| {
            format!("Launcher {release_version} does not include a portable Windows executable.")
        })?;
    *state.pending_update.lock().map_err(error_string)? = Some(PendingUpdate {
        asset: asset.clone(),
        version: release_version.clone(),
    });
    emit_updater(
        app,
        "available",
        format!("Launcher {release_version} is available."),
    );
    Ok(())
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

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> LauncherResult<LauncherSnapshot> {
    get_snapshot(state.inner())
}

#[tauri::command]
async fn choose_folder(state: State<'_, AppState>) -> LauncherResult<Option<LauncherSnapshot>> {
    let backend = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let current = load_config()?.install_path.unwrap();
        let selected = rfd::FileDialog::new()
            .set_title("Choose OpenShores install folder")
            .set_directory(current)
            .pick_folder();
        match selected {
            Some(path) => {
                save_install_path(&path)?;
                get_snapshot(&backend).map(Some)
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
fn launch_game(app: AppHandle, state: State<'_, AppState>) -> LauncherResult<LauncherSnapshot> {
    launch_game_sync(&app, state.inner())
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
    ];
    if !allowed.iter().any(|prefix| url.starts_with(prefix)) {
        return Err("Blocked external URL.".to_string());
    }
    open::that(url).map_err(error_string)
}

#[tauri::command]
async fn check_updates(app: AppHandle, state: State<'_, AppState>) -> LauncherResult<()> {
    let backend = state.inner().clone();
    let app_for_error = app.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || check_updates_sync(&app, &backend, true))
            .await
            .map_err(error_string)?;
    if let Err(error) = &result {
        emit_updater(
            &app_for_error,
            "error",
            format!("Update check unavailable: {error}"),
        );
    }
    result
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

#[tauri::command]
fn window_minimize(window: WebviewWindow) -> LauncherResult<()> {
    window.minimize().map_err(error_string)
}

#[tauri::command]
fn window_maximize(window: WebviewWindow) -> LauncherResult<()> {
    if window.is_maximized().map_err(error_string)? {
        window.unmaximize().map_err(error_string)
    } else {
        window.maximize().map_err(error_string)
    }
}

#[tauri::command]
fn window_close(window: WebviewWindow) -> LauncherResult<()> {
    window.close().map_err(error_string)
}

fn error_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_state,
            choose_folder,
            install_game,
            uninstall_game,
            launch_game,
            open_folder,
            open_link,
            check_updates,
            install_launcher_update,
            window_minimize,
            window_maximize,
            window_close
        ])
        .setup(|app| {
            cleanup_legacy_electron_data()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
            let webview_data = launcher_data_path()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?
                .join("webview");
            let window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("OpenShores Launcher")
                    .inner_size(800.0, 640.0)
                    .min_inner_size(700.0, 560.0)
                    .center()
                    .decorations(false)
                    .transparent(false)
                    .shadow(false)
                    .data_directory(webview_data)
                    .build()?;
            #[cfg(windows)]
            hide_native_window_border(&window);
            let app_handle = app.handle().clone();
            let backend = app.state::<AppState>().inner().clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(1));
                report_previous_update_error(&app_handle);
                thread::sleep(Duration::from_secs(2));
                let _ = check_updates_sync(&app_handle, &backend, false);
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
    fn release_versions_accept_a_v_prefix() {
        let release = GithubRelease {
            tag_name: "v1.2.3".to_string(),
            name: None,
            published_at: None,
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

    #[test]
    fn xdelta_payload_is_embedded() {
        assert!(XDELTA_BYTES.len() > 500_000);
    }
}
