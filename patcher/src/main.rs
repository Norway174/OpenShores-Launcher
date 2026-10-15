#[path = "../../src-tauri/src/chain.rs"]
mod chain;
#[path = "../../src-tauri/src/install.rs"]
mod install;

use install::{InstallResult, Site, LOGIN_EXPIRED};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    env,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

const DEFAULT_HOST: &str = "openshores.net";
const SESSION_COOKIE: &str = "sessionid";
const LAUNCHER_HEADER: &str = "X-OpenShores-Launcher";
const USAGE: &str = "\
Installs and updates OpenShores

  openshores-patcher update <folder>   install, or update to the latest version
  openshores-patcher check <folder>    say which version the folder holds and whether a newer one exists
  openshores-patcher login             log in to the website and remember it
  openshores-patcher logout            forget the login

Options:
  --host <name>       the website (default openshores.net)
  --user <name>       account name, instead of being asked
  --hpatchz <file>    the HDiffPatch patch program to use, instead of the built-in or installed one

The password is asked for, or taken from the OPENSHORES_PASSWORD variable.
On Linux, start OpenShores.exe in the folder with Wine.";

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    host: String,
    #[serde(default)]
    session: String,
}

struct Options {
    command: String,
    folder: Option<PathBuf>,
    host: String,
    user: Option<String>,
    hpatchz: Option<PathBuf>,
}

fn error_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn data_path() -> InstallResult<PathBuf> {
    #[cfg(windows)]
    let base = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share")));
    base.map(|base| base.join("OpenShores-Launcher"))
        .ok_or_else(|| "Could not find a folder to keep the patcher's files in.".to_string())
}

fn saved_path() -> InstallResult<PathBuf> {
    Ok(data_path()?.join("patcher.json"))
}

fn is_session_value(value: &str) -> bool {
    (16..=128).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn is_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
}

fn parse(arguments: &[String]) -> InstallResult<Options> {
    let mut options = Options {
        command: String::new(),
        folder: None,
        host: DEFAULT_HOST.to_string(),
        user: None,
        hpatchz: None,
    };
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        let mut value = |name: &str| {
            words
                .next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value."))
        };
        match word.as_str() {
            "--host" => options.host = value("--host")?,
            "--user" => options.user = Some(value("--user")?),
            "--hpatchz" => options.hpatchz = Some(PathBuf::from(value("--hpatchz")?)),
            "-h" | "--help" | "help" => options.command = "help".to_string(),
            other if other.starts_with('-') => return Err(format!("Unknown option {other}.")),
            other if options.command.is_empty() => options.command = other.to_string(),
            other if options.folder.is_none() => options.folder = Some(PathBuf::from(other)),
            other => return Err(format!("Unexpected argument {other}.")),
        }
    }
    if !is_host(&options.host) {
        return Err("--host takes the website's name, like openshores.net.".to_string());
    }
    Ok(options)
}

fn http(session: Option<&str>) -> InstallResult<Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(session) = session {
        let mut cookie = reqwest::header::HeaderValue::from_str(&format!("{SESSION_COOKIE}={session}"))
            .map_err(error_string)?;
        cookie.set_sensitive(true);
        headers.insert(reqwest::header::COOKIE, cookie);
    }
    Client::builder()
        .user_agent(format!("OpenShores-Patcher/{}", env!("OPENSHORES_LAUNCHER_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .default_headers(headers)
        .build()
        .map_err(error_string)
}

fn ask(prompt: &str) -> InstallResult<String> {
    print!("{prompt}");
    io::stdout().flush().map_err(error_string)?;
    let mut line = String::new();
    io::stdin().read_line(&mut line).map_err(error_string)?;
    Ok(line.trim().to_string())
}

fn login(options: &Options) -> InstallResult<String> {
    let user = match &options.user {
        Some(user) => user.clone(),
        None => ask(&format!("Account name on {}: ", options.host))?,
    };
    if user.is_empty() {
        return Err("No account name was given.".to_string());
    }
    let password = match env::var("OPENSHORES_PASSWORD") {
        Ok(password) if !password.is_empty() => password,
        _ => rpassword::prompt_password("Password: ").map_err(error_string)?,
    };
    let response = http(None)?
        .post(format!("https://{}/api/launcher/login", options.host))
        .header(LAUNCHER_HEADER, "1")
        .timeout(Duration::from_secs(30))
        .json(&json!({ "username": user, "password": password }))
        .send()
        .map_err(|_| "Could not reach the website.".to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response
            .json::<Value>()
            .ok()
            .and_then(|value| value.get("detail").and_then(Value::as_str).map(str::to_string));
        return Err(detail.unwrap_or_else(|| format!("Login failed ({status}).")));
    }
    let session = response
        .cookies()
        .find(|cookie| cookie.name() == SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string())
        .filter(|value| is_session_value(value))
        .ok_or_else(|| "The website accepted the login but gave no session.".to_string())?;
    install::write_json(
        &saved_path()?,
        &Saved {
            host: options.host.clone(),
            session: session.clone(),
        },
    )?;
    Ok(session)
}

fn saved_session(options: &Options) -> Option<String> {
    let saved: Saved = install::read_json(&saved_path().ok()?)?;
    (saved.host.eq_ignore_ascii_case(&options.host) && is_session_value(&saved.session)).then_some(saved.session)
}

fn fetch_chain(options: &Options, session: &str) -> InstallResult<Site> {
    let client = http(Some(session))?;
    let origin = format!("https://{}", options.host);
    let response = client
        .get(format!("{origin}/api/patches"))
        .timeout(Duration::from_secs(30))
        .send()
        .map_err(|_| "Could not reach the website.".to_string())?;
    match response.status().as_u16() {
        401 | 403 => return Err(LOGIN_EXPIRED.to_string()),
        404 => return Err("There are no downloads right now.".to_string()),
        _ => {}
    }
    if !response.status().is_success() {
        return Err(format!("Patch list download failed ({}).", response.status()));
    }
    let chain: chain::Chain = response
        .json()
        .map_err(|_| "The website's patch list could not be read.".to_string())?;
    chain.validate()?;
    Ok(Site { client, origin, chain })
}

fn site(options: &Options) -> InstallResult<Site> {
    if let Some(session) = saved_session(options) {
        match fetch_chain(options, &session) {
            Err(error) if error == LOGIN_EXPIRED => println!("The saved login has expired."),
            other => return other,
        }
    }
    let session = login(options)?;
    fetch_chain(options, &session)
}

fn describe(chain: &chain::Chain, folder: &Path) -> InstallResult<(String, bool)> {
    let latest = chain
        .version(&chain.latest)
        .map(|version| version.title.clone())
        .unwrap_or_else(|| chain.latest.clone());
    Ok(match chain.identify(folder)? {
        chain::Installed::Empty => (format!("Nothing is installed there. The latest version is {latest}."), true),
        chain::Installed::Unknown => (
            "The folder holds files that are not a known version of OpenShores.".to_string(),
            false,
        ),
        chain::Installed::Version(id) if id == chain.latest => (format!("{latest} is installed and up to date."), false),
        chain::Installed::Version(id) => {
            let title = chain.version(&id).map(|version| version.title.clone()).unwrap_or(id);
            (format!("{title} is installed. {latest} is available."), true)
        }
    })
}

fn absolute(folder: &Path) -> InstallResult<PathBuf> {
    std::path::absolute(folder).map_err(error_string)
}

fn run(options: &Options) -> InstallResult<()> {
    match options.command.as_str() {
        "" | "help" => {
            println!("openshores-patcher {}\n\n{USAGE}", env!("OPENSHORES_LAUNCHER_VERSION"));
            Ok(())
        }
        "login" => {
            login(options)?;
            println!("Logged in.");
            Ok(())
        }
        "logout" => {
            let _ = std::fs::remove_file(saved_path()?);
            println!("The login is forgotten.");
            Ok(())
        }
        "check" => {
            let folder = absolute(options.folder.as_deref().ok_or("check needs the game folder.")?)?;
            let access = site(options)?;
            println!("{}", describe(&access.chain, &folder)?.0);
            Ok(())
        }
        "update" | "install" => {
            let folder = absolute(options.folder.as_deref().ok_or("update needs the game folder.")?)?;
            let access = site(options)?;
            let data = data_path()?;
            let downloads = data.join("patcher-downloads");
            let tool = || match &options.hpatchz {
                Some(tool) if tool.is_file() => Ok(tool.clone()),
                Some(tool) => Err(format!("{} is not a file.", tool.display())),
                None => install::ensure_hpatchz(&data),
            };

            let live = io::stdout().is_terminal();
            let last = RefCell::new((String::new(), -1_i64));
            let report = |phase: &str, percent: f64, detail: String| {
                let mut last = last.borrow_mut();
                let whole = percent as i64;
                let new_phase = last.0 != phase;
                if live {
                    if new_phase && !last.0.is_empty() {
                        println!();
                    }
                    print!("\r\x1b[2K[{whole:>3}%] {phase}: {detail}");
                    let _ = io::stdout().flush();
                } else if new_phase || whole >= last.1 + 10 {
                    println!("[{whole:>3}%] {phase}: {detail}");
                } else {
                    return;
                }
                *last = (phase.to_string(), whole);
            };
            let result = install::install(&report, &access, &tool, &downloads, folder.clone());
            if live && !last.borrow().0.is_empty() {
                println!();
            }
            if result? {
                println!("OpenShores is installed in {}.", folder.display());
            } else {
                println!("OpenShores in {} is already the latest version.", folder.display());
            }
            if cfg!(not(windows)) {
                println!("To play: wine \"{}\"", folder.join("OpenShores.exe").display());
            }
            Ok(())
        }
        other => Err(format!("Unknown command {other}. Try: openshores-patcher help")),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    match parse(&arguments).and_then(|options| run(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_command_line_is_read() {
        let options = parse(&words("update /games/os --host test.example --user Me --hpatchz /bin/hpatchz")).unwrap();
        assert_eq!(options.command, "update");
        assert_eq!(options.folder, Some(PathBuf::from("/games/os")));
        assert_eq!(options.host, "test.example");
        assert_eq!(options.user.as_deref(), Some("Me"));
        assert_eq!(options.hpatchz, Some(PathBuf::from("/bin/hpatchz")));
        assert_eq!(parse(&words("check here")).unwrap().host, DEFAULT_HOST);
    }

    #[test]
    fn a_bad_command_line_is_refused() {
        assert!(parse(&words("update a b")).is_err());
        assert!(parse(&words("update a --host")).is_err());
        assert!(parse(&words("update a --what")).is_err());
        assert!(parse(&words("update a --host evil.example/path")).is_err());
        assert!(parse(&words("update a --host user@evil.example")).is_err());
    }

    #[test]
    fn only_a_plain_session_value_is_kept() {
        assert!(is_session_value("abcdefghijklmnop1234"));
        assert!(!is_session_value("short"));
        assert!(!is_session_value("abcdefghijklmnop; other=1"));
    }
}
