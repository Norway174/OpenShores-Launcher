use crate::chain;
use reqwest::{blocking::Client, Url};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    env,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub type InstallResult<T> = Result<T, String>;

pub type Report<'a> = dyn Fn(&str, f64, String) + 'a;

fn emit(report: &Report, phase: &str, percent: f64, detail: impl Into<String>) {
    report(phase, percent, detail.into());
}

fn error_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub const DOWNLOAD_ATTEMPTS: u32 = 4;
pub const SPACE_MARGIN: u64 = 100 * 1024 * 1024;
pub const LOGIN_EXPIRED: &str = "Your login has expired. Log in again to download and update the game.";
pub const MANIFEST_FILE: &str = ".openshores-launcher.json";
pub const MANAGED_BY: &str = "OpenShores Launcher";

pub struct Site {
    pub client: Client,
    pub origin: String,
    pub chain: chain::Chain,
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> InstallResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(error_string)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(error_string)?;
    fs::write(path, text).map_err(error_string)
}

pub fn is_managed_manifest(manifest: Option<&Value>) -> bool {
    manifest
        .and_then(|value| value.get("managedBy"))
        .and_then(Value::as_str)
        == Some(MANAGED_BY)
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
    }
}

enum DownloadFailure {
    Retry(String),
    Fatal(String),
}

fn download_attempt(
    report: &Report,
    client: &Client,
    url: &str,
    destination: &Path,
    expected: u64,
    phase: &str,
    range_start: f64,
    range_end: f64,
) -> Result<(), DownloadFailure> {
    use DownloadFailure::{Fatal, Retry};
    let mut have = fs::metadata(destination).map(|found| found.len()).unwrap_or(0);
    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().map_err(|error| Retry(error_string(error)))?;
    let status = response.status().as_u16();
    if status == 416 {
        let _ = fs::remove_file(destination);
        return Err(Retry("the file changed on the website".to_string()));
    }
    if !response.status().is_success() {
        return Err(Fatal(format!("Download failed ({}).", response.status())));
    }
    let mut output = if status == 206 && have > 0 {
        File::options().append(true).open(destination)
    } else {
        have = 0;
        File::create(destination)
    }
    .map_err(|error| Fatal(error_string(error)))?;
    let total = if expected > 0 {
        expected
    } else {
        have + response.content_length().unwrap_or(0)
    };
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut received = have;
    loop {
        let count = response.read(&mut buffer).map_err(|error| Retry(error_string(error)))?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).map_err(|error| Fatal(error_string(error)))?;
        received += count as u64;
        let ratio = if total > 0 {
            (received as f64 / total as f64).min(1.0)
        } else {
            0.0
        };
        let detail = if total > 0 {
            format!("{} of {}", format_bytes(received), format_bytes(total))
        } else {
            format_bytes(received)
        };
        emit(report,
            phase,
            range_start + ratio * (range_end - range_start),
            detail,
        );
    }
    output.flush().map_err(|error| Fatal(error_string(error)))?;
    if expected > 0 && received < expected {
        return Err(Retry("the connection closed early".to_string()));
    }
    Ok(())
}

pub fn download(
    report: &Report,
    client: &Client,
    url: &str,
    destination: &Path,
    expected: u64,
    phase: &str,
    range_start: f64,
    range_end: f64,
) -> InstallResult<()> {
    if expected > 0 && fs::metadata(destination).is_ok_and(|found| found.len() > expected) {
        let _ = fs::remove_file(destination);
    }
    let mut last = String::new();
    for attempt in 0..DOWNLOAD_ATTEMPTS {
        if attempt > 0 {
            emit(report, phase, range_start, format!("Connection lost ({last}). Continuing..."));
            thread::sleep(Duration::from_secs(2 * attempt as u64));
        }
        match download_attempt(report, client, url, destination, expected, phase, range_start, range_end) {
            Ok(()) => return Ok(()),
            Err(DownloadFailure::Fatal(error)) => return Err(error),
            Err(DownloadFailure::Retry(error)) => last = error,
        }
    }
    Err(format!(
        "The download kept breaking off ({last}). Try again."
    ))
}

pub fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos:x}", std::process::id())
}

pub fn fetch_chain_file(
    report: &Report,
    access: &Site,
    downloads: &Path,
    url: &str,
    file: &str,
    size: u64,
    sha256: &str,
    phase: &str,
    range_start: f64,
    range_end: f64,
) -> InstallResult<PathBuf> {
    let folder = downloads.to_path_buf();
    fs::create_dir_all(&folder).map_err(error_string)?;
    let destination = folder.join(file);
    let complete = |path: &Path| -> InstallResult<bool> {
        Ok(path.is_file()
            && fs::metadata(path).map_err(error_string)?.len() == size
            && chain::sha256_file(path)?.eq_ignore_ascii_case(sha256))
    };
    if complete(&destination)? {
        emit(report, phase, range_end, format!("{file} is already downloaded."));
        return Ok(destination);
    }
    let partial = folder.join(format!("{file}.part"));
    let address = Url::parse(&format!("{}{url}", access.origin)).map_err(error_string)?;
    download(
        report,
        &access.client,
        address.as_str(),
        &partial,
        size,
        phase,
        range_start,
        range_end,
    )
    .map_err(|error| {
        if error.contains("401") || error.contains("403") {
            LOGIN_EXPIRED.to_string()
        } else if error.contains("429") {
            "Too many downloads were started in the last hour. Try again later.".to_string()
        } else {
            error
        }
    })?;
    emit(report, phase, range_end, format!("Checking {file}..."));
    if !complete(&partial)? {
        let _ = fs::remove_file(&partial);
        return Err(format!("{file} did not arrive intact. Try again."));
    }
    let _ = fs::remove_file(&destination);
    fs::rename(&partial, &destination).map_err(error_string)?;
    Ok(destination)
}

pub fn bytes_to_download(folder: &Path, file: &str, size: u64) -> u64 {
    let length = |name: String| fs::metadata(folder.join(name)).map(|found| found.len()).unwrap_or(0);
    if length(file.to_string()) == size {
        0
    } else {
        size.saturating_sub(length(format!("{file}.part")))
    }
}

pub fn require_free_space(folder: &Path, needed: u64, purpose: &str) -> InstallResult<()> {
    let Ok(free) = fs2::available_space(folder) else { return Ok(()) };
    if free >= needed {
        return Ok(());
    }
    Err(format!(
        "Not enough free disk space {purpose}: about {} is needed on the drive of {}, and {} is free.",
        format_bytes(needed),
        folder.display(),
        format_bytes(free)
    ))
}

#[cfg(windows)]
pub const HPATCHZ_NAME: &str = "hpatchz.exe";
#[cfg(not(windows))]
pub const HPATCHZ_NAME: &str = "hpatchz";

#[cfg(windows)]
pub const HPATCHZ_BYTES: Option<&[u8]> = Some(include_bytes!("../../resources/hdiffpatch/hpatchz.exe"));
#[cfg(all(not(windows), bundled_hpatchz))]
pub const HPATCHZ_BYTES: Option<&[u8]> = Some(include_bytes!("../../resources/hdiffpatch/hpatchz-linux-x86_64"));
#[cfg(all(not(windows), not(bundled_hpatchz)))]
pub const HPATCHZ_BYTES: Option<&[u8]> = None;

pub fn ensure_hpatchz(data_dir: &Path) -> InstallResult<PathBuf> {
    let Some(bytes) = HPATCHZ_BYTES else {
        return find_on_path(HPATCHZ_NAME).ok_or_else(|| {
            "The patch program hpatchz (HDiffPatch) is not installed: install it, or pass its place with --hpatchz."
                .to_string()
        });
    };
    let path = data_dir.join("app").join("resources").join(HPATCHZ_NAME);
    let must_write = match fs::read(&path) {
        Ok(existing) => existing != bytes,
        Err(_) => true,
    };
    if must_write {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(error_string)?;
        }
        fs::write(&path, bytes).map_err(error_string)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(error_string)?;
    }
    Ok(path)
}

pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths).map(|folder| folder.join(name)).find(|candidate| candidate.is_file())
}

pub fn manifest_files(manifest: Option<&Value>) -> Option<HashSet<String>> {
    if !is_managed_manifest(manifest) {
        return None;
    }
    let files = manifest?.get("files")?.as_array()?;
    Some(
        files
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

pub fn write_launcher_manifest(
    folder: &Path,
    version: &chain::Version,
    files: Option<&HashSet<String>>,
) -> InstallResult<()> {
    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(error_string)?;
    let mut manifest = json!({
        "managedBy": MANAGED_BY,
        "launcherVersion": env!("OPENSHORES_LAUNCHER_VERSION"),
        "installedAt": installed_at,
        "version": version.id,
        "title": version.title,
    });
    if let Some(files) = files {
        let mut sorted: Vec<&String> = files.iter().collect();
        sorted.sort();
        manifest
            .as_object_mut()
            .unwrap()
            .insert("files".to_string(), json!(sorted));
    }
    write_json(&folder.join(MANIFEST_FILE), &manifest)
}

pub fn directory_file_bytes(path: &Path) -> InstallResult<u64> {
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

pub fn install(
    report: &Report,
    access: &Site,
    tool: &dyn Fn() -> InstallResult<PathBuf>,
    downloads_dir: &Path,
    install_path: PathBuf,
) -> InstallResult<bool> {
    let chain = &access.chain;
    let latest = chain
        .version(&chain.latest)
        .cloned()
        .ok_or_else(|| "The patch list does not describe its latest version.".to_string())?;
    let found = chain.identify(&install_path)?;
    let old_manifest = read_json::<Value>(&install_path.join(MANIFEST_FILE));
    let fresh = found == chain::Installed::Empty;
    let start = match &found {
        chain::Installed::Empty => chain
            .base
            .as_ref()
            .map(|base| base.version.clone())
            .ok_or_else(|| "The website does not offer the original client right now.".to_string())?,
        chain::Installed::Version(id) => {
            let same_program = chain
                .version(id)
                .is_some_and(|version| version.exe.eq_ignore_ascii_case(&latest.exe));
            if !same_program {
                return Err("This folder holds the original 2018 client, which the launcher leaves alone. Choose an empty folder; the launcher downloads what it needs.".to_string());
            }
            id.clone()
        }
        chain::Installed::Unknown => {
            return Err("The selected folder holds files that are not a known version of OpenShores. Choose an empty folder, or the folder of an existing OpenShores installation.".to_string());
        }
    };
    let patches = chain.route(&start).ok_or_else(|| {
        format!(
            "No patches lead from version {start} to version {}.",
            chain.latest
        )
    })?;

    if !fresh && patches.is_empty() {
        write_launcher_manifest(&install_path, &latest, manifest_files(old_manifest.as_ref()).as_ref())?;
        emit(report,
            "Ready to play",
            100.0,
            format!("{} is up to date.", latest.title),
        );
        return Ok(false);
    }

    let parent = install_path
        .parent()
        .ok_or_else(|| "The installation folder has no parent directory.".to_string())?;
    fs::create_dir_all(parent).map_err(error_string)?;

    let downloads = downloads_dir.to_path_buf();
    fs::create_dir_all(&downloads).map_err(error_string)?;
    let fresh_base = if fresh { chain.base.as_ref() } else { None };
    let to_download = patches
        .iter()
        .map(|patch| bytes_to_download(&downloads, &patch.file, patch.size))
        .sum::<u64>()
        + fresh_base.map_or(0, |base| bytes_to_download(&downloads, &base.file, base.size));
    require_free_space(&downloads, to_download + SPACE_MARGIN, "for the downloads")?;
    let game_size = match fresh_base {
        Some(base) => base.size * 2,
        None => directory_file_bytes(&install_path).unwrap_or(0),
    };
    let to_build = fresh_base.map_or(0, |base| base.size * 2)
        + patches.iter().map(|patch| patch.size * 2).sum::<u64>()
        + game_size * if patches.len() > 1 { 2 } else { 1 };
    require_free_space(parent, to_build + SPACE_MARGIN, "to install the game")?;

    let token = unique_token();
    let work = parent.join(format!(".openshores-staging-{token}"));
    let backup_path = parent.join(format!(".openshores-backup-{token}"));
    let mut backup_active = false;
    let mut downloaded: Vec<PathBuf> = Vec::new();

    let result = (|| {
        fs::create_dir_all(&work).map_err(error_string)?;
        let tool = tool()?;
        let base = if fresh { chain.base.as_ref() } else { None };
        let download_total = (patches.iter().map(|patch| patch.size).sum::<u64>()
            + base.map_or(0, |base| base.size))
        .max(1) as f64;
        let mut received = 0_u64;
        let mut span = |size: u64| {
            let from = 2.0 + received as f64 / download_total * 58.0;
            received += size;
            (from, 2.0 + received as f64 / download_total * 58.0)
        };

        let mut base_archive = None;
        if let Some(base) = base {
            let (from, to) = span(base.size);
            let archive = fetch_chain_file(
                report,
                access,
                &downloads,
                &base.url,
                &base.file,
                base.size,
                &base.sha256,
                "Downloading the original client",
                from,
                to,
            )?;
            downloaded.push(archive.clone());
            base_archive = Some(archive);
        }
        let mut archives = Vec::new();
        for (index, patch) in patches.iter().enumerate() {
            let (from, to) = span(patch.size);
            let archive = fetch_chain_file(
                report,
                access,
                &downloads,
                &patch.url,
                &patch.file,
                patch.size,
                &patch.sha256,
                &format!("Downloading patch {} of {}", index + 1, patches.len()),
                from,
                to,
            )?;
            downloaded.push(archive.clone());
            archives.push(archive);
        }

        let mut current = install_path.clone();
        let mut stage: Option<PathBuf> = None;
        if let (Some(base), Some(archive)) = (base, base_archive.as_ref()) {
            emit(report,
                "Unpacking the original client",
                61.0,
                format!("Unpacking {}...", base.file),
            );
            let unpacked = work.join("base");
            chain::extract_archive(archive, &unpacked)?;
            let base_exe = chain
                .version(&base.version)
                .map(|version| version.exe.clone())
                .unwrap_or_default();
            current = chain::find_folder_with(&unpacked, &base_exe)
                .ok_or_else(|| format!("{} does not hold the original client.", base.file))?;
            if chain.identify(&current)? != chain::Installed::Version(base.version.clone()) {
                return Err(format!(
                    "{} is not the version of the original client the patches start from.",
                    base.file
                ));
            }
            stage = Some(unpacked);
        }

        let mut files: Option<HashSet<String>> = None;
        for (index, (patch, archive)) in patches.iter().zip(&archives).enumerate() {
            emit(report,
                "Patching",
                62.0 + index as f64 / patches.len() as f64 * 30.0,
                format!(
                    "Version {} to {} (step {} of {})...",
                    patch.from,
                    patch.to,
                    index + 1,
                    patches.len()
                ),
            );
            let unpacked = work.join(format!("patch-{index}"));
            chain::extract_archive(archive, &unpacked)?;
            let patch_root = chain::find_folder_with(&unpacked, chain::PATCH_FILE)
                .ok_or_else(|| format!("{} does not hold a patch.", patch.file))?;
            let output = work.join(format!("game-{index}"));
            chain::apply_patch(
                &tool,
                &current,
                &patch_root.join(chain::PATCH_FILE),
                &output,
                &work.join(format!("patch-{index}.log")),
            )?;
            let target = chain
                .version(&patch.to)
                .ok_or_else(|| format!("The patch list does not describe version {}.", patch.to))?;
            let executable = output.join(&target.exe);
            if !executable.is_file()
                || !chain::sha256_file(&executable)?.eq_ignore_ascii_case(&target.exe_sha256)
            {
                return Err(format!(
                    "Patching to version {} did not give the expected game.",
                    patch.to
                ));
            }
            let dates = patch_root.join(chain::DATES_FILE);
            files = if dates.is_file() {
                Some(chain::restore_dates(&dates, &output)?)
            } else {
                None
            };
            let _ = fs::remove_dir_all(&unpacked);
            if let Some(previous) = stage.replace(output.clone()) {
                let _ = fs::remove_dir_all(previous);
            }
            current = output;
        }
        if stage.is_none() {
            return Err("There was nothing to install.".to_string());
        }

        emit(report,
            "Finishing",
            94.0,
            "Putting the new version in place...",
        );
        if !fresh {
            chain::carry_over(
                &install_path,
                &current,
                manifest_files(old_manifest.as_ref()).as_ref(),
            )?;
        }
        write_launcher_manifest(&current, &latest, files.as_ref())?;
        if install_path.exists() {
            fs::rename(&install_path, &backup_path).map_err(|error| {
                format!("The game folder is in use and could not be replaced ({error}). Close anything open in it and try again.")
            })?;
            backup_active = true;
        }
        fs::rename(&current, &install_path).map_err(error_string)?;
        if backup_active {
            backup_active = false;
            let _ = fs::remove_dir_all(&backup_path);
        }
        emit(report,
            "Ready to play",
            100.0,
            format!("{} is installed.", latest.title),
        );
        Ok(true)
    })();

    if result.is_err() && backup_active && backup_path.exists() && !install_path.exists() {
        let _ = fs::rename(&backup_path, &install_path);
    }
    let _ = fs::remove_dir_all(&work);
    if result.is_ok() {
        for archive in downloaded {
            let _ = fs::remove_file(archive);
        }
    }
    result
}
