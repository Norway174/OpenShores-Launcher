'use strict';

const $ = selector => document.querySelector(selector);
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
window.launcher = {
  getState: () => invoke('get_state'),
  chooseFolder: () => invoke('choose_folder'),
  install: () => invoke('install_game'),
  getPatchReleases: () => invoke('get_ip_patch_releases'),
  setPatchRelease: selection => invoke('set_ip_patch_release', { selection }),
  updatePatch: () => invoke('update_ip_patch'),
  uninstall: () => invoke('uninstall_game'),
  launch: () => invoke('launch_game'),
  openFolder: () => invoke('open_folder'),
  openLink: url => invoke('open_link', { url }),
  checkUpdates: () => invoke('check_updates'),
  installUpdate: () => invoke('install_launcher_update'),
  minimize: () => invoke('window_minimize'),
  maximize: () => invoke('window_maximize'),
  close: () => invoke('window_close'),
  onProgress: callback => listen('operation-progress', event => callback(event.payload)),
  onOperationStatus: callback => listen('operation-status', event => callback(event.payload)),
  onGameStatus: callback => listen('game-status', event => callback(event.payload)),
  onStateChanged: callback => listen('state-changed', event => callback(event.payload)),
  onUpdaterStatus: callback => listen('updater-status', event => callback(event.payload))
};
let state = null;
let busy = false;
let updateModalLocked = false;
let updateModalPreviousFocus = null;

function showError(error) {
  const box = $('#error-box');
  box.textContent = error?.message || String(error);
  box.classList.remove('hidden');
}

function clearError() { $('#error-box').classList.add('hidden'); }

function setUpdateCheckResult(message, status = '') {
  const result = $('#update-check-result');
  result.textContent = message;
  result.className = `update-check-result${status ? ` ${status}` : ''}`;
  result.classList.toggle('hidden', !message);
}

function openUpdateModal() {
  const modal = $('#update-modal');
  const wasHidden = modal.classList.contains('hidden');
  if (wasHidden) updateModalPreviousFocus = document.activeElement;
  modal.classList.remove('hidden');
  if (wasHidden) window.setTimeout(() => $('#install-update').focus(), 0);
}

function closeUpdateModal() {
  if (updateModalLocked) return;
  $('#update-modal').classList.add('hidden');
  if (updateModalPreviousFocus?.focus) updateModalPreviousFocus.focus();
}

function setUpdateModalLocked(locked) {
  updateModalLocked = locked;
  $('#dismiss-update').disabled = locked;
  $('#update-later').classList.toggle('hidden', locked);
}

function render(nextState = state) {
  if (!nextState) return;
  state = nextState;
  $('.version').textContent = `v${state.launcherVersion}`;
  $('#section-nav').classList.remove('hidden');
  $('#startup-placeholder').classList.add('hidden');
  $('#actions').classList.remove('hidden');
  $('#install-path').textContent = state.installPath;
  $('#patch-channel').textContent = state.ipPatchRelease === 'latest' ? 'Latest Release' : state.ipPatchRelease;
  $('#patch-release').value = state.ipPatchRelease || 'latest';
  $('#patch-badge').classList.toggle('hidden', !state.installed);
  $('#open-folder').classList.toggle('hidden', !state.installed);
  $('#uninstall').disabled = !state.installed || busy;
  $('#refresh-game').disabled = !state.installed || busy || state.gameRunning;
  $('#update-patch').disabled = !state.installed || busy || state.gameRunning;
  $('#patch-release').disabled = busy;
  $('#choose-folder').disabled = busy || state.installed;
  const primary = $('#primary-action');
  const dot = $('#status-dot');
  dot.className = 'large-dot';
  if (state.gameRunning) {
    $('#status-text').textContent = 'OpenShores is running';
    dot.classList.add('running');
    primary.textContent = 'Game running';
    primary.disabled = true;
  } else if (state.installed) {
    $('#status-text').textContent = 'Ready to play';
    dot.classList.add('ready');
    primary.textContent = 'Launch OpenShores';
    primary.disabled = busy;
  } else {
    $('#status-text').textContent = 'Not installed';
    primary.textContent = 'Install OpenShores';
    primary.disabled = busy;
  }
}

async function runOperation(operation, showProgress = true) {
  if (busy) return;
  busy = true;
  clearError();
  if (showProgress) $('#progress-area').classList.remove('hidden');
  render();
  try {
    state = { ...state, ...(await operation()) };
    render();
  } catch (error) {
    showError(error);
    try {
      state = { ...state, ...(await window.launcher.getState()) };
    } catch (_) {
      // Keep the last known state when a refresh is unavailable.
    }
  } finally {
    busy = false;
    render();
  }
}

window.launcher.onProgress(data => {
  $('#progress-area').classList.remove('hidden');
  $('#progress-phase').textContent = data.phase;
  $('#progress-percent').textContent = `${data.percent}%`;
  $('#progress-bar').style.width = `${data.percent}%`;
  $('#progress-detail').textContent = data.detail;
});

window.launcher.onOperationStatus(data => {
  busy = data.busy;
  if (data.error) {
    $('#progress-phase').textContent = 'IP patch update failed';
    $('#progress-percent').textContent = 'Failed';
    $('#progress-bar').style.width = '0';
    $('#progress-detail').textContent = data.error;
    showError(data.error);
  }
  render();
});

window.launcher.onGameStatus(data => {
  state.gameRunning = data.running;
  if (data.error) showError(data.error);
  render();
});

window.launcher.onStateChanged(render);

function handleUpdaterStatus(data) {
  const installButton = $('#install-update');
  if (data.state === 'available') {
    setUpdateModalLocked(false);
    $('#update-dialog-title').textContent = 'A new version is available';
    $('#update-message').textContent = data.message;
    installButton.textContent = 'Download & restart';
    installButton.disabled = false;
    setUpdateCheckResult(data.message);
    openUpdateModal();
  } else if (data.state === 'downloading' || data.state === 'installing') {
    setUpdateModalLocked(true);
    $('#update-dialog-title').textContent = data.state === 'downloading' ? 'Downloading update' : 'Installing update';
    $('#update-message').textContent = data.message;
    installButton.textContent = data.state === 'downloading' ? 'Downloading...' : 'Restarting...';
    installButton.disabled = true;
    openUpdateModal();
  } else if (data.state === 'current') {
    setUpdateCheckResult(data.message, 'success');
  } else if (data.state === 'error') {
    setUpdateModalLocked(false);
    setUpdateCheckResult(data.message, 'error');
    if (!$('#update-modal').classList.contains('hidden')) {
      $('#update-dialog-title').textContent = 'Update could not finish';
      $('#update-message').textContent = data.message;
      installButton.textContent = 'Try again';
      installButton.disabled = false;
    }
  }
}

window.launcher.onUpdaterStatus(handleUpdaterStatus);

document.querySelectorAll('.nav-item').forEach(button => button.addEventListener('click', () => {
  document.querySelectorAll('.nav-item').forEach(item => item.classList.toggle('active', item === button));
  document.querySelectorAll('.view').forEach(view => view.classList.remove('active'));
  $(`#${button.dataset.view}-view`).classList.add('active');
}));

$('#primary-action').addEventListener('click', () => state.installed ? runOperation(window.launcher.launch, false) : runOperation(window.launcher.install));
$('#open-folder').addEventListener('click', () => window.launcher.openFolder().catch(showError));
$('#choose-folder').addEventListener('click', async () => { clearError(); try { const next = await window.launcher.chooseFolder(); if (next) render(next); } catch (error) { showError(error); } });
$('#uninstall').addEventListener('click', () => runOperation(window.launcher.uninstall));
$('#refresh-game').addEventListener('click', () => runOperation(window.launcher.install));
$('#update-patch').addEventListener('click', () => runOperation(window.launcher.updatePatch));
$('#patch-release').addEventListener('change', event => {
  const selection = event.currentTarget.value;
  state = { ...state, ipPatchRelease: selection };
  runOperation(() => window.launcher.setPatchRelease(selection));
});
$('#check-updates').addEventListener('click', async event => {
  const button = event.currentTarget;
  if (button.disabled) return;
  button.disabled = true;
  button.textContent = 'Checking...';
  setUpdateCheckResult('Checking for launcher updates...', 'checking');
  try {
    handleUpdaterStatus(await window.launcher.checkUpdates());
  } catch (error) {
    setUpdateCheckResult(error?.message || String(error), 'error');
  } finally {
    button.disabled = false;
    button.textContent = 'Check now';
  }
});
$('#install-update').addEventListener('click', async () => {
  if ($('#install-update').disabled) return;
  setUpdateModalLocked(true);
  $('#install-update').disabled = true;
  $('#install-update').textContent = 'Starting...';
  try {
    await window.launcher.installUpdate();
  } catch (error) {
    setUpdateModalLocked(false);
    $('#update-dialog-title').textContent = 'Update could not finish';
    $('#update-message').textContent = error?.message || String(error);
    $('#install-update').disabled = false;
    $('#install-update').textContent = 'Try again';
  }
});
$('#update-later').addEventListener('click', closeUpdateModal);
$('#dismiss-update').addEventListener('click', closeUpdateModal);
$('#update-modal').addEventListener('click', event => { if (event.target === event.currentTarget) closeUpdateModal(); });
document.addEventListener('keydown', event => { if (event.key === 'Escape') closeUpdateModal(); });
$('#minimize').addEventListener('click', () => window.launcher.minimize());
$('#close').addEventListener('click', () => window.launcher.close());
document.querySelectorAll('[data-url]').forEach(button => button.addEventListener('click', () => window.launcher.openLink(button.dataset.url).catch(showError)));

async function loadPatchReleases() {
  const select = $('#patch-release');
  try {
    const releases = await window.launcher.getPatchReleases();
    const selected = state?.ipPatchRelease || 'latest';
    select.replaceChildren(new Option('Latest Release', 'latest'));
    for (const release of releases) {
      const date = release.publishedAt
        ? new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' }).format(new Date(release.publishedAt))
        : 'Unknown date';
      const option = new Option(`${release.name} (${release.tag}) — ${date}`, release.tag);
      option.disabled = !release.hasZip;
      if (!release.hasZip) option.textContent += ' — no ZIP asset';
      select.add(option);
    }
    select.value = selected;
  } catch (error) {
    showError(`Could not load IP patch releases: ${error?.message || String(error)}`);
  } finally {
    select.disabled = busy;
  }
}

window.launcher.getState().then(nextState => {
  render(nextState);
  loadPatchReleases();
}).catch(error => {
  $('#startup-placeholder').querySelector('strong').textContent = 'Startup could not finish';
  $('#startup-placeholder').querySelector('p').textContent = 'The error below can be selected and copied.';
  $('.startup-spinner').classList.add('hidden');
  $('#status-text').textContent = 'Launcher needs attention';
  showError(error);
});
