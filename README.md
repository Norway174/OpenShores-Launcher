# OpenShores Launcher

![OpenShores space banner](assets/figma/space-banner.png)

A fast, portable Windows and Linux launcher for [OpenShores](https://openshores.net/), built with Tauri. It downloads the official game client, applies the latest compatible [OpenShores IP Patch](https://github.com/Celarious/OpenShores-IP-Patch), launches the game, and manages updates and uninstallation from a compact native interface.

## Features

- Downloads the official OpenShores client from its per-file manifest and verifies every clean source file with SHA-256.
- Reuses unchanged clean files during game refreshes and downloads only missing or changed files.
- Fetches release ZIPs for the IP patch, preserves their folder structure, and applies every compatible xdelta with xdelta3.
- Lets users follow the latest IP patch release or pin a specific published release.
- Checks the selected IP patch in the background at startup and once per hour, without downloading the game again.
- Launches the game and keeps the installed state after the game exits.
- Refreshes or removes the launcher-managed game installation.
- Checks IP patch and launcher update channels independently.
- Updates the portable launcher executable in place—no installer required.
- Stores settings and working data under:
    - Windows: `%LOCALAPPDATA%\OpenShores-Launcher`
    - Linux: `$HOME/.local/OpenShores-Launcher`
- Defaults the game installation to:
    - Windows: `%LOCALAPPDATA%\OpenShores`
    - Linux: `$HOME/.local/OpenShores`

## Using the launcher

### Requirements

#### All Platforms
- An internet connection for game installation and update checks.

#### Windows
- Windows 10 or Windows 11, 64-bit.
- Microsoft Edge WebView2 Runtime. It is included with current Windows releases and does not normally need to be installed separately.

#### Linux
- Any 64-bit x86 Linux distribution
- glibc 2.35+
- xdelta3
- wine
    - The chosen WINEPREFIX must have VS2015 (MSVC140) installed in it for the IP Patch to work.

### Run it

1. Download the proper version for your operating system from the repository's **Releases** page.
2. Move the executable to a permanent, user-writable location, such as a folder under Documents or `%LOCALAPPDATA%`. Avoid `Program Files`, because the portable self-updater must be able to replace the executable. (Optional)
3. Double-click the executable. No installation required.
4. Select **Install OpenShores**. The launcher downloads and verifies the clean client files, downloads the selected IP patch, and applies it automatically.
5. Select **Launch OpenShores** when the status changes to **Ready to play**.

The game installation folder can be changed from **Settings** before installation. Errors displayed by the launcher can be selected and copied when reporting a problem.

### Files created by the launcher

The portable executable remains wherever you placed it. Launcher-specific data is stored separately:

| Location (Windows)                                | Location (Linux)                              | Purpose                                                       |
| ------------------------------------------------- | --------------------------------------------- | ------------------------------------------------------------- |
| `%LOCALAPPDATA%\OpenShores-Launcher\config.json`  | `$HOME/.local/OpenShores-Launcher/config.json | Launcher settings and the configured game path                |
| `%LOCALAPPDATA%\OpenShores-Launcher\temp`         | `$HOME/.local/OpenShores-Launcher/temp        | Downloads, update files, and temporary replacement scripts    |
| `%LOCALAPPDATA%\OpenShores-Launcher\webview`      | `$HOME/.local/OpenShores-Launcher/webview     | WebView2 application data                                     |
| `%LOCALAPPDATA%\OpenShores`                       | `$HOME/.local/OpenShores                      | Default managed game installation                             |

Selecting **Uninstall OpenShores** removes the managed game installation, not the launcher itself. To remove the launcher completely, close it, delete its portable `.exe` or `.AppImage`, and optionally delete `%LOCALAPPDATA%\OpenShores-Launcher` / `$HOME/.local/OpenShores-Launcher` if you also want to remove its settings and cached data.

## Screenshots
![Application Preview](assets/appPreview.png)

## Contributing

Contributions and issue reports are welcome. Development currently targets 64-bit Windows and 64-bit x86 Linux.

### Development requirements

Install the following before cloning the project:

#### Windows

- [Git for Windows](https://git-scm.com/download/win)
- [Node.js](https://nodejs.org/) with npm
- The stable [Rust MSVC toolchain](https://www.rust-lang.org/tools/install)
- Microsoft Visual Studio Build Tools with the **Desktop development with C++** workload
- Microsoft Edge WebView2 Runtime

#### Linux

- `git`
- `rust` (stable-x86_64-unknown-linux-gnu)
    - [`tauri-cli` crate](https://crates.io/crates/tauri-cli)
- `npm`
- `webkit2gtk`
- `gtk3`

### Clone and run

#### Windows
```powershell
git clone https://github.com/Norway174/OpenShores-Launcher.git
cd OpenShores-Launcher
npm install
npm start
```

`npm start` compiles the Rust backend in development mode and launches the Tauri application. The frontend has no JavaScript package dependencies; npm is used as a convenient entry point for the PowerShell build scripts.

#### Linux
```bash
git clone https://github.com/Norway174/OpenShores-Launcher.git
cd OpenShores-Launcher
cargo tauri dev
```

`cargo tauri dev` launches the Tauri application in development mode, with hot-reloading for Rust code.

### Test and build

```powershell
npm test
npm run dist
```

`npm test` runs the Rust test suite. `npm run dist` creates the optimized portable executable at:

```text
dist\OpenShores-Launcher.exe
```

`package.json` is the sole source of truth for the launcher version. Tauri reads it directly, and the Rust build embeds the same value in the executable. Cargo's internal package version remains `0.0.0` and is not used as the launcher version. Every push to `main` checks the `package.json` version and only creates a GitHub release when it does not already have a matching tag; ordinary commits run without publishing a release.

To publish a new launcher, update the version in `package.json`, commit it with the release changes, and push to `main`. GitHub Actions builds the Windows executable, creates a matching `v<version>` release, and uploads the portable executable. The launcher verifies updates with the SHA-256 digest supplied by GitHub Releases.

The release executable uses the static MSVC C runtime and embeds the frontend, application icon, and xdelta3 utility. Windows supplies WebView2, which keeps the portable binary substantially smaller than an Electron bundle.

### Project layout

| Path | Purpose |
| --- | --- |
| `src-tauri/src/main.rs` | Native launcher backend, downloads, patching, process management, and updates |
| `src/renderer` | HTML, CSS, JavaScript, and runtime image assets embedded by Tauri |
| `src-tauri/tauri.conf.json` | Tauri application and security configuration |
| `resources/xdelta3` | Embedded xdelta3 executable and its GPLv2 license |
| `scripts` | Development, test, and portable release build scripts |
| `assets` | Source artwork and design exports |
| `build/icon.ico` | Windows executable icon used by the release build |

Do not commit `src-tauri/target`, `src-tauri/gen`, `dist`, or `node_modules`; these are generated locally and excluded by `.gitignore`.

## Downloads, updates, and licensing

- Game client manifest: [openshores.net/downloads/manifest.json](https://openshores.net/downloads/manifest.json)
- IP patch releases: [Celarious/OpenShores-IP-Patch](https://github.com/Celarious/OpenShores-IP-Patch)
- Launcher updates: releases from `Norway174/OpenShores-Launcher`

Binary patch application uses the embedded xdelta3 3.0.11 executable on Windows. xdelta3 is distributed under GPLv2, and its license is included at `resources/xdelta3/COPYING`. On Linux, the system is expected to have `xdelta3` available in the path.

## Disclaimer

This launcher was vibe-coded with AI. Although it has been reviewed and tested, AI-assisted code can contain mistakes; use it at your own risk and review contributions carefully.
