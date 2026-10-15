use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub type ChainResult<T> = Result<T, String>;

pub const CHAIN_FORMAT: u32 = 1;
pub const PATCH_FILE: &str = "openshores.patch";
pub const DATES_FILE: &str = "openshores.patch2";
pub const LAUNCHER_FILE_PREFIX: &str = ".openshores-";
const DOWNLOAD_URL_PREFIX: &str = "/api/download/";
const PATCH_TIMEOUT: Duration = Duration::from_secs(30 * 60);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TICKS_AT_UNIX_EPOCH: i128 = 621_355_968_000_000_000;

#[derive(Clone, Debug, Deserialize)]
pub struct Chain {
    pub format: u32,
    pub latest: String,
    pub versions: Vec<Version>,
    pub base: Option<BaseFile>,
    #[serde(default)]
    pub patches: Vec<PatchFile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Version {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub exe: String,
    pub exe_sha256: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BaseFile {
    pub version: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PatchFile {
    pub from: String,
    pub to: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Installed {
    Empty,
    Version(String),
    Unknown,
}

fn error_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn is_safe_name(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b" ._+()-".contains(&byte))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_download_url(value: &str) -> bool {
    value.starts_with(DOWNLOAD_URL_PREFIX)
        && value.len() > DOWNLOAD_URL_PREFIX.len()
        && !value[DOWNLOAD_URL_PREFIX.len()..].contains(['/', '\\', '?', '#'])
        && !value.contains("..")
}

impl Chain {
    pub fn validate(&self) -> ChainResult<()> {
        if self.format != CHAIN_FORMAT {
            return Err(
                "This launcher is too old for the server's patch list. Update the launcher.".to_string(),
            );
        }
        let mut ids = HashSet::new();
        for version in &self.versions {
            if !is_safe_name(&version.id, 32)
                || !is_safe_name(&version.exe, 120)
                || !is_sha256(&version.exe_sha256)
                || !ids.insert(version.id.as_str())
            {
                return Err(format!("The patch list describes version '{}' badly.", version.id));
            }
        }
        if !ids.contains(self.latest.as_str()) {
            return Err("The patch list does not describe its latest version.".to_string());
        }
        if let Some(base) = &self.base {
            if !ids.contains(base.version.as_str())
                || !is_safe_name(&base.file, 120)
                || !is_sha256(&base.sha256)
                || !is_download_url(&base.url)
            {
                return Err("The patch list describes the original client badly.".to_string());
            }
        }
        for patch in &self.patches {
            if !ids.contains(patch.from.as_str())
                || !ids.contains(patch.to.as_str())
                || patch.from == patch.to
                || !is_safe_name(&patch.file, 120)
                || !is_sha256(&patch.sha256)
                || !is_download_url(&patch.url)
            {
                return Err(format!("The patch list describes '{}' badly.", patch.file));
            }
        }
        Ok(())
    }

    pub fn version(&self, id: &str) -> Option<&Version> {
        self.versions.iter().find(|version| version.id == id)
    }

    pub fn route(&self, from: &str) -> Option<Vec<PatchFile>> {
        let mut best: HashMap<&str, (u64, Vec<&PatchFile>)> = HashMap::new();
        best.insert(from, (0, Vec::new()));
        let mut todo = vec![from];
        while !todo.is_empty() {
            let index = (0..todo.len()).min_by_key(|index| best[todo[*index]].0)?;
            let here = todo.swap_remove(index);
            let (size, path) = best[here].clone();
            if here == self.latest {
                return Some(path.into_iter().cloned().collect());
            }
            for patch in self.patches.iter().filter(|patch| patch.from == here) {
                let total = size.saturating_add(patch.size);
                if best.get(patch.to.as_str()).is_none_or(|known| total < known.0) {
                    let mut next = path.clone();
                    next.push(patch);
                    best.insert(patch.to.as_str(), (total, next));
                    if !todo.contains(&patch.to.as_str()) {
                        todo.push(patch.to.as_str());
                    }
                }
            }
        }
        None
    }

    pub fn identify(&self, folder: &Path) -> ChainResult<Installed> {
        let has_files = fs::read_dir(folder)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);
        if !has_files {
            return Ok(Installed::Empty);
        }
        let mut checksums: HashMap<String, Option<String>> = HashMap::new();
        for version in self.versions.iter().rev() {
            let key = version.exe.to_ascii_lowercase();
            if !checksums.contains_key(&key) {
                let path = folder.join(&version.exe);
                let checksum = if path.is_file() {
                    Some(sha256_file(&path)?)
                } else {
                    None
                };
                checksums.insert(key.clone(), checksum);
            }
            if checksums[&key]
                .as_deref()
                .is_some_and(|checksum| checksum.eq_ignore_ascii_case(&version.exe_sha256))
            {
                return Ok(Installed::Version(version.id.clone()));
            }
        }
        Ok(Installed::Unknown)
    }
}

pub fn sha256_file(path: &Path) -> ChainResult<String> {
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

pub fn extract_archive(archive: &Path, destination: &Path) -> ChainResult<()> {
    fs::create_dir_all(destination).map_err(error_string)?;
    let extension = archive
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "zip" => {
            let input = File::open(archive).map_err(error_string)?;
            let mut zipped = zip::ZipArchive::new(input).map_err(error_string)?;
            for index in 0..zipped.len() {
                let mut entry = zipped.by_index(index).map_err(error_string)?;
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
        "7z" => sevenz_rust2::decompress_file(archive, destination).map_err(error_string),
        _ => Err(format!(
            "{} is not a .zip or .7z archive.",
            archive.file_name().unwrap_or_default().to_string_lossy()
        )),
    }
}

pub fn find_folder_with(root: &Path, name: &str) -> Option<PathBuf> {
    let mut level = vec![root.to_path_buf()];
    for _ in 0..4 {
        let mut next = Vec::new();
        for folder in &level {
            if folder.join(name).is_file() {
                return Some(folder.clone());
            }
            if let Ok(entries) = fs::read_dir(folder) {
                next.extend(
                    entries
                        .flatten()
                        .map(|entry| entry.path())
                        .filter(|path| path.is_dir()),
                );
            }
        }
        level = next;
    }
    None
}

pub fn apply_patch(tool: &Path, old: &Path, patch: &Path, output: &Path, log: &Path) -> ChainResult<()> {
    if output.exists() {
        return Err(format!("{} already exists.", output.display()));
    }
    let log_file = File::create(log).map_err(error_string)?;
    let mut command = Command::new(tool);
    command
        .arg("-f")
        .arg(old)
        .arg(patch)
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone().map_err(error_string)?))
        .stderr(Stdio::from(log_file));
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .map_err(|error| format!("The patcher could not be started: {error}"))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(error_string)? {
            break status;
        }
        if started.elapsed() >= PATCH_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Patching timed out after {} minutes.",
                PATCH_TIMEOUT.as_secs() / 60
            ));
        }
        thread::sleep(Duration::from_millis(200));
    };
    if status.success() {
        return Ok(());
    }
    let printed = fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = printed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(3)
        .collect();
    let detail = tail.into_iter().rev().collect::<Vec<_>>().join(" ");
    Err(format!(
        "The patch could not be applied ({status}). The game files it started from are changed or incomplete. {detail}"
    ))
}

fn relative_path(value: &str) -> Option<(PathBuf, String)> {
    let mut path = PathBuf::new();
    let mut key = Vec::new();
    for segment in value.split(['\\', '/']) {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains(':') {
            return None;
        }
        path.push(segment);
        key.push(segment.to_lowercase());
    }
    if key.is_empty() {
        None
    } else {
        Some((path, key.join("/")))
    }
}

fn ticks_to_time(ticks: i128) -> Option<SystemTime> {
    let since_epoch = ticks - TICKS_AT_UNIX_EPOCH;
    if since_epoch < 0 {
        return None;
    }
    let seconds = u64::try_from(since_epoch / 10_000_000).ok()?;
    let nanoseconds = (since_epoch % 10_000_000) as u32 * 100;
    UNIX_EPOCH.checked_add(Duration::new(seconds, nanoseconds))
}

pub fn restore_dates(dates_file: &Path, root: &Path) -> ChainResult<HashSet<String>> {
    let text = fs::read_to_string(dates_file).map_err(error_string)?;
    let mut files = HashSet::new();
    for line in text.lines() {
        let mut parts = line.trim_start_matches('\u{feff}').splitn(3, '|');
        let (Some(kind), Some(ticks), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if kind != "F" {
            continue;
        }
        let Some((relative, key)) = relative_path(name.trim_end()) else {
            continue;
        };
        files.insert(key);
        let Some(when) = ticks.parse::<i128>().ok().and_then(ticks_to_time) else {
            continue;
        };
        if let Ok(file) = File::options().write(true).open(root.join(relative)) {
            let _ = file.set_modified(when);
        }
    }
    Ok(files)
}

fn walk_files(root: &Path, folder: &Path, found: &mut Vec<(PathBuf, String)>) -> ChainResult<()> {
    for entry in fs::read_dir(folder).map_err(error_string)? {
        let path = entry.map_err(error_string)?.path();
        if path.is_dir() {
            walk_files(root, &path, found)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            let key = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
                .collect::<Vec<_>>()
                .join("/");
            found.push((relative.to_path_buf(), key));
        }
    }
    Ok(())
}

pub fn carry_over(old: &Path, new: &Path, known: Option<&HashSet<String>>) -> ChainResult<usize> {
    let mut found = Vec::new();
    walk_files(old, old, &mut found)?;
    let mut copied = 0;
    for (relative, key) in found {
        let name = relative
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with(LAUNCHER_FILE_PREFIX) || known.is_some_and(|known| known.contains(&key)) {
            continue;
        }
        let target = new.join(&relative);
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(error_string)?;
        }
        fs::copy(old.join(&relative), &target).map_err(error_string)?;
        copied += 1;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "openshores-launcher-test-{name}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn put(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn sha(content: &str) -> String {
        format!("{:x}", Sha256::digest(content.as_bytes()))
    }

    fn chain() -> Chain {
        serde_json::from_value(serde_json::json!({
            "format": 1,
            "latest": "1.0.3",
            "versions": [
                {"id": "2018", "title": "2018 client", "exe": "Shores of Hazeron.exe", "exe_sha256": sha("old")},
                {"id": "1.0.1", "exe": "OpenShores.exe", "exe_sha256": sha("one")},
                {"id": "1.0.2", "exe": "OpenShores.exe", "exe_sha256": sha("two")},
                {"id": "1.0.3", "exe": "OpenShores.exe", "exe_sha256": sha("three")}
            ],
            "base": {"version": "2018", "file": "base.7z", "size": 50, "sha256": sha("b"), "url": "/api/download/base.7z"},
            "patches": [
                {"from": "2018", "to": "1.0.1", "file": "p1.zip", "size": 30, "sha256": sha("1"), "url": "/api/download/p1.zip"},
                {"from": "1.0.1", "to": "1.0.2", "file": "p2.zip", "size": 20, "sha256": sha("2"), "url": "/api/download/p2.zip"},
                {"from": "1.0.2", "to": "1.0.3", "file": "p3.zip", "size": 10, "sha256": sha("3"), "url": "/api/download/p3.zip"}
            ]
        }))
        .unwrap()
    }

    fn names(route: Option<Vec<PatchFile>>) -> Option<Vec<String>> {
        route.map(|patches| patches.into_iter().map(|patch| patch.file).collect())
    }

    #[test]
    fn patches_are_followed_in_order() {
        let chain = chain();
        chain.validate().unwrap();
        assert_eq!(names(chain.route("2018")), Some(vec!["p1.zip".into(), "p2.zip".into(), "p3.zip".into()]));
        assert_eq!(names(chain.route("1.0.2")), Some(vec!["p3.zip".into()]));
        assert_eq!(names(chain.route("1.0.3")), Some(vec![]));
        assert_eq!(names(chain.route("0.9")), None);
    }

    #[test]
    fn a_smaller_way_round_is_preferred_and_a_dead_end_is_not_taken() {
        let mut chain = chain();
        chain.patches.push(PatchFile {
            from: "2018".into(),
            to: "1.0.2".into(),
            file: "roll.zip".into(),
            size: 35,
            sha256: sha("r"),
            url: "/api/download/roll.zip".into(),
        });
        assert_eq!(names(chain.route("2018")), Some(vec!["roll.zip".into(), "p3.zip".into()]));
        chain.patches.retain(|patch| patch.file != "p3.zip");
        assert_eq!(names(chain.route("2018")), None);
    }

    #[test]
    fn a_bad_description_is_refused() {
        let good = chain();
        let mut bad = good.clone();
        bad.format = 2;
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.latest = "9.9".into();
        assert!(bad.validate().is_err());
        for url in ["https://elsewhere.example/p1.zip", "/api/download/../secret", "/api/download/a/b.zip", "/static/p1.zip"] {
            let mut bad = good.clone();
            bad.patches[0].url = url.into();
            assert!(bad.validate().is_err(), "{url}");
        }
        for file in ["../p1.zip", "sub/p1.zip", ".hidden", ""] {
            let mut bad = good.clone();
            bad.patches[0].file = file.into();
            assert!(bad.validate().is_err(), "{file}");
        }
        let mut bad = good.clone();
        bad.patches[0].to = "2018".into();
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.versions[1].exe = "..\\OpenShores.exe".into();
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.versions[2].id = "1.0.1".into();
        assert!(bad.validate().is_err());
        let mut bad = good;
        bad.base.as_mut().unwrap().sha256 = "short".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn a_folder_is_recognised_by_its_executable() {
        let chain = chain();
        let root = scratch("identify");
        assert_eq!(chain.identify(&root.join("missing")).unwrap(), Installed::Empty);
        assert_eq!(chain.identify(&root).unwrap(), Installed::Empty);
        put(&root.join("readme.txt"), "x");
        assert_eq!(chain.identify(&root).unwrap(), Installed::Unknown);
        put(&root.join("OpenShores.exe"), "two");
        assert_eq!(chain.identify(&root).unwrap(), Installed::Version("1.0.2".into()));
        put(&root.join("OpenShores.exe"), "changed by hand");
        assert_eq!(chain.identify(&root).unwrap(), Installed::Unknown);
        put(&root.join("Shores of Hazeron.exe"), "old");
        assert_eq!(chain.identify(&root).unwrap(), Installed::Version("2018".into()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dates_are_restored_and_the_file_list_returned() {
        let root = scratch("dates");
        put(&root.join("OpenShores.exe"), "exe");
        put(&root.join("GD").join("data.har"), "data");
        let ticks = TICKS_AT_UNIX_EPOCH + 1_704_164_645_i128 * 10_000_000;
        let listing = format!(
            "\u{feff}D|{ticks}|GD\r\nF|{ticks}|OpenShores.exe\r\nF|{ticks}|GD\\data.har\r\nF|{ticks}|..\\outside.txt\r\nF|nonsense|gone.txt\r\nbroken line\r\n"
        );
        put(&root.join("openshores.patch2"), &listing);
        let files = restore_dates(&root.join("openshores.patch2"), &root).unwrap();
        assert_eq!(files, HashSet::from(["openshores.exe".to_string(), "gd/data.har".to_string(), "gone.txt".to_string()]));
        let expected = UNIX_EPOCH + Duration::from_secs(1_704_164_645);
        for name in ["OpenShores.exe", "GD/data.har"] {
            assert_eq!(fs::metadata(root.join(name)).unwrap().modified().unwrap(), expected, "{name}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_players_own_files_are_carried_over() {
        let root = scratch("carry");
        let (old, new) = (root.join("old"), root.join("new"));
        put(&old.join("OpenShores.exe"), "one");
        put(&old.join("Sounds").join("dropped.wav"), "removed by the update");
        put(&old.join("Screenshots").join("mine.png"), "player file");
        put(&old.join("notes.txt"), "player file");
        put(&old.join(".openshores-launcher.json"), "{}");
        put(&new.join("OpenShores.exe"), "two");
        put(&new.join("notes.txt"), "shipped with the new version");
        let known = HashSet::from(["openshores.exe".to_string(), "sounds/dropped.wav".to_string()]);
        assert_eq!(carry_over(&old, &new, Some(&known)).unwrap(), 1);
        assert_eq!(fs::read_to_string(new.join("Screenshots").join("mine.png")).unwrap(), "player file");
        assert!(!new.join("Sounds").join("dropped.wav").exists());
        assert!(!new.join(".openshores-launcher.json").exists());
        assert_eq!(fs::read_to_string(new.join("notes.txt")).unwrap(), "shipped with the new version");
        assert_eq!(fs::read_to_string(new.join("OpenShores.exe")).unwrap(), "two");
        assert_eq!(carry_over(&old, &new, None).unwrap(), 1);
        assert!(new.join("Sounds").join("dropped.wav").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore]
    fn real_release_installs_from_the_original_client() {
        let release = PathBuf::from(std::env::var("OPENSHORES_TEST_RELEASE").unwrap());
        let work = PathBuf::from(std::env::var("OPENSHORES_TEST_WORK").unwrap());
        let tool = PathBuf::from(std::env::var("OPENSHORES_TEST_HPATCHZ").unwrap());
        let text = fs::read_to_string(release.join("patches.json")).unwrap();
        let mut list: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap();
        for entry in list["patches"].as_array_mut().unwrap().iter_mut() {
            let file = entry["file"].as_str().unwrap().to_string();
            entry["size"] = fs::metadata(release.join(&file)).unwrap().len().into();
            entry["url"] = format!("/api/download/{file}").into();
        }
        let file = list["base"]["file"].as_str().unwrap().to_string();
        list["base"]["size"] = fs::metadata(release.join(&file)).unwrap().len().into();
        list["base"]["url"] = format!("/api/download/{file}").into();
        let chain: Chain = serde_json::from_value(list).unwrap();
        chain.validate().unwrap();

        let base = chain.base.clone().unwrap();
        assert_eq!(sha256_file(&release.join(&base.file)).unwrap(), base.sha256, "{}", base.file);
        let unpacked = work.join("base");
        extract_archive(&release.join(&base.file), &unpacked).unwrap();
        let base_exe = chain.version(&base.version).unwrap().exe.clone();
        let mut current = find_folder_with(&unpacked, &base_exe).expect("the original client in its archive");
        assert_eq!(chain.identify(&current).unwrap(), Installed::Version(base.version.clone()));

        let patches = chain.route(&base.version).expect("patches from the original client to the latest version");
        assert!(!patches.is_empty());
        for (index, patch) in patches.iter().enumerate() {
            assert_eq!(sha256_file(&release.join(&patch.file)).unwrap(), patch.sha256, "{}", patch.file);
            let files = work.join(format!("patch-{index}"));
            extract_archive(&release.join(&patch.file), &files).unwrap();
            let root = find_folder_with(&files, PATCH_FILE).expect("openshores.patch in the patch archive");
            let output = work.join(format!("game-{index}"));
            apply_patch(&tool, &current, &root.join(PATCH_FILE), &output, &work.join(format!("patch-{index}.log"))).unwrap();
            assert_eq!(chain.identify(&output).unwrap(), Installed::Version(patch.to.clone()));
            let listed = restore_dates(&root.join(DATES_FILE), &output).unwrap();
            let mut on_disk = Vec::new();
            walk_files(&output, &output, &mut on_disk).unwrap();
            let on_disk: HashSet<String> = on_disk.into_iter().map(|(_, key)| key).collect();
            assert_eq!(listed, on_disk, "the dates file lists exactly the files of version {}", patch.to);
            current = output;
        }
        assert_eq!(chain.identify(&current).unwrap(), Installed::Version(chain.latest.clone()));
        println!("installed {} into {}", chain.latest, current.display());
    }

    #[test]
    fn archives_are_unpacked_and_the_game_folder_found() {
        use std::io::Write;
        let root = scratch("archive");
        let zip_path = root.join("patch.zip");
        let mut writer = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("Shores of Hazeron/Shores of Hazeron.exe", options).unwrap();
        writer.write_all(b"old").unwrap();
        writer.start_file("Shores of Hazeron/GD/data.har", options).unwrap();
        writer.write_all(b"data").unwrap();
        writer.finish().unwrap();
        let out = root.join("out");
        extract_archive(&zip_path, &out).unwrap();
        assert_eq!(find_folder_with(&out, "Shores of Hazeron.exe"), Some(out.join("Shores of Hazeron")));
        assert_eq!(find_folder_with(&out, "missing.exe"), None);
        put(&root.join("notes.rar"), "x");
        assert!(extract_archive(&root.join("notes.rar"), &root.join("no")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
