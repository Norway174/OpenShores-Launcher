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
  addServer: (nickname, host) => invoke('add_server', { nickname, host }),
  editServer: (serverId, nickname, host) => invoke('edit_server', { serverId, nickname, host }),
  removeServer: serverId => invoke('remove_server', { serverId }),
  connectServer: serverId => invoke('connect_server', { serverId }),
  refreshServerStatuses: () => invoke('refresh_server_statuses'),
  loginAccount: (username, password) => invoke('login_account', { username, password }),
  registerAccount: (username, password) => invoke('register_account', { username, password }),
  logoutAccount: () => invoke('logout_account'),
  launch: () => invoke('launch_game'),
  launchOfflineDesigner: () => invoke('launch_offline_designer'),
  stopProcess: process => invoke('stop_game_process', { process }),
  openFolder: () => invoke('open_folder'),
  openLink: url => invoke('open_link', { url }),
  checkUpdates: () => invoke('check_updates'),
  installUpdate: () => invoke('install_launcher_update'),
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
let serverStatusTimer = null;
let serverRefreshBusy = false;
let registering = false;
const launchingProcesses = new Set();
let pendingStopProcess = null;
let stopModalPreviousFocus = null;

function connectedServer() {
  return state?.servers?.find(server => server.id === state.connectedServerId) || null;
}

function statusLabel(value) {
  return value === 'online' ? 'Online' : value === 'offline' ? 'Offline' : 'Unavailable';
}

function renderServiceState() {
  const indicator = $('#service-state');
  const server = connectedServer();
  const online = !!server && ['login', 'scene', 'chat'].every(service => server.status?.[service] === 'online');
  indicator.classList.toggle('online', online);
  indicator.classList.toggle('offline', !online);
  indicator.querySelector('span').textContent = online ? 'ONLINE' : server ? 'OFFLINE' : 'NOT CONNECTED';
}

function makeStatus(name, value) {
  const row = document.createElement('div');
  row.className = 'server-service';
  const label = document.createElement('span');
  label.textContent = name;
  const stateLabel = document.createElement('strong');
  stateLabel.className = `service-value ${value === 'online' ? 'online' : value === 'offline' ? 'offline' : 'unknown'}`;
  stateLabel.textContent = statusLabel(value);
  row.append(label, stateLabel);
  return row;
}

function renderServers() {
  const list = $('#server-list');
  list.replaceChildren();
  const servers = state.servers || [];
  $('#server-empty').classList.toggle('hidden', servers.length > 0);
  for (const server of servers) {
    const card = document.createElement('article');
    card.className = `server-card${server.id === state.connectedServerId ? ' connected' : ''}`;
    const top = document.createElement('div');
    top.className = 'server-card-top';
    const identity = document.createElement('div');
    const title = document.createElement('h2');
    title.textContent = server.nickname;
    const host = document.createElement('p');
    host.textContent = server.host;
    identity.append(title, host);
    const online = ['login', 'scene', 'chat'].every(service => server.status?.[service] === 'online');
    const connect = document.createElement('button');
    connect.className = server.id === state.connectedServerId ? 'secondary connected-button' : 'primary compact';
    connect.dataset.action = 'connect';
    connect.dataset.serverId = server.id;
    connect.textContent = server.id === state.connectedServerId ? 'Disconnect' : 'Connect';
    connect.disabled = server.id !== state.connectedServerId && !online;
    connect.title = connect.disabled ? 'This server must be online before you can connect.' : '';
    const controls = document.createElement('div');
    controls.className = 'server-card-controls';
    const tools = document.createElement('div');
    tools.className = 'server-card-tools';
    const edit = document.createElement('button');
    edit.className = 'icon-button';
    edit.dataset.action = 'edit';
    edit.dataset.serverId = server.id;
    edit.setAttribute('aria-label', `Edit ${server.nickname}`);
    edit.title = 'Edit server';
    edit.textContent = 'Edit';
    const remove = document.createElement('button');
    remove.className = 'icon-button danger-text';
    remove.dataset.action = 'remove';
    remove.dataset.serverId = server.id;
    remove.setAttribute('aria-label', `Remove ${server.nickname}`);
    remove.title = 'Remove server';
    remove.textContent = '×';
    tools.append(edit, remove);
    controls.append(tools, connect);
    top.append(identity, controls);
    const services = document.createElement('div');
    services.className = 'server-services';
    services.append(
      makeStatus('Login Server', server.status?.login),
      makeStatus('Scene Server', server.status?.scene),
      makeStatus('Chat Server', server.status?.chat)
    );
    card.append(top, services);
    list.append(card);
  }
}

function renderAccount() {
  const server = connectedServer();
  const tab = $('#account-tab');
  tab.disabled = !server;
  $('#account-tab-wrap').classList.toggle('tooltip-enabled', !server);
  if (!server && tab.classList.contains('active')) switchView('home');
  $('#account-server-copy').textContent = server ? `Connected to ${server.nickname} · ${server.host}` : 'Connect to a server to continue.';
  $('#account-login-title').textContent = server ? `Login to ${server.nickname}` : 'Login';
  $('#account-login-address').textContent = server?.host || '—';
  const loggedIn = !!state.accountUsername;
  $('#account-logged-out').classList.toggle('hidden', loggedIn);
  $('#account-logged-in').classList.toggle('hidden', !loggedIn);
  $('#account-name').textContent = state.accountUsername || '';
}

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

function renderProcessButton(button, process, running, idleLabel, enabled) {
  const launching = launchingProcesses.has(process);
  button.classList.toggle('process-running', running);
  button.textContent = running ? 'Running...' : launching ? 'Launching...' : idleLabel;
  button.disabled = launching || (!running && (!enabled || busy));
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
  $('#game-source').textContent = connectedServer()?.host || 'No server selected';
  const anyProcessRunning = state.gameRunning || state.designerRunning;
  $('#open-folder').disabled = !state.installed || busy;
  $('#uninstall').disabled = !state.installed || busy;
  $('#refresh-game').disabled = !state.installed || busy || anyProcessRunning;
  $('#update-patch').disabled = !state.installed || busy || anyProcessRunning;
  $('#patch-release').disabled = busy;
  $('#choose-folder').disabled = busy || anyProcessRunning;
  $('#choose-folder').textContent = state.installed ? 'Move…' : 'Browse…';
  renderServers();
  renderAccount();
  renderServiceState();
  const primary = $('#primary-action');
  const designer = $('#offline-designer');
  const dot = $('#status-dot');
  dot.className = 'large-dot';
  if (anyProcessRunning) {
    $('#status-text').textContent = state.gameRunning && state.designerRunning
      ? 'OpenShores and Offline Designer are running'
      : state.gameRunning ? 'OpenShores is running' : 'Offline Designer is running';
    dot.classList.add('running');
  } else if (state.installed) {
    $('#status-text').textContent = 'Ready to play';
    dot.classList.add('ready');
  } else {
    $('#status-text').textContent = 'Not installed';
  }
  renderProcessButton(
    primary,
    'game',
    !!state.gameRunning,
    state.installed ? 'Launch OpenShores' : 'Install OpenShores',
    true
  );
  renderProcessButton(designer, 'designer', !!state.designerRunning, 'Open Offline Designer', state.installed);
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

async function launchProcess(process, operation) {
  if (launchingProcesses.has(process)) return;
  launchingProcesses.add(process);
  clearError();
  render();
  try {
    state = { ...state, ...(await operation()) };
  } catch (error) {
    showError(error);
    try {
      state = { ...state, ...(await window.launcher.getState()) };
    } catch (_) {
      // Keep the last known state when a refresh is unavailable.
    }
  } finally {
    launchingProcesses.delete(process);
    render();
  }
}

window.launcher.onProgress(data => {
  $('#progress-area').classList.remove('hidden');
  $('#progress-phase').textContent = data.phase;
  $('#progress-percent').textContent = `${data.percent}%`;
  $('#progress-bar').style.width = `${data.percent}%`;
  $('#progress-bar').classList.toggle('complete', data.percent >= 100);
  $('#progress-detail').textContent = data.detail;
});

window.launcher.onOperationStatus(data => {
  busy = data.busy;
  if (data.error) {
    $('#progress-phase').textContent = 'IP patch update failed';
    $('#progress-percent').textContent = 'Failed';
    $('#progress-bar').style.width = '0';
    $('#progress-bar').classList.remove('complete');
    $('#progress-detail').textContent = data.error;
    showError(data.error);
  }
  render();
});

window.launcher.onGameStatus(data => {
  if (data.process === 'designer') state.designerRunning = data.running;
  else state.gameRunning = data.running;
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

function switchView(viewName) {
  const button = document.querySelector(`.nav-item[data-view="${viewName}"]`);
  if (!button || button.disabled) return;
  document.querySelectorAll('.nav-item').forEach(item => item.classList.toggle('active', item === button));
  document.querySelectorAll('.view').forEach(view => view.classList.remove('active'));
  $(`#${viewName}-view`).classList.add('active');
}

document.querySelectorAll('.nav-item').forEach(button => button.addEventListener('click', () => switchView(button.dataset.view)));

function showServerDialog(server = null) {
  $('#server-edit-id').value = server?.id || '';
  $('#server-nickname').value = server?.nickname || '';
  $('#server-host').value = server?.host || '';
  $('#server-dialog-title').textContent = server ? 'Edit server' : 'Add server';
  $('#server-modal-error').classList.add('hidden');
  $('#server-modal').classList.remove('hidden');
  window.setTimeout(() => $('#server-nickname').focus(), 0);
}

function closeServerDialog() { $('#server-modal').classList.add('hidden'); }

async function updateServerState(operation, errorTarget = '#server-error') {
  const error = $(errorTarget);
  error.classList.add('hidden');
  try {
    render(await operation());
    return true;
  } catch (reason) {
    error.textContent = reason?.message || String(reason);
    error.classList.remove('hidden');
    return false;
  }
}

async function withMinimumDelay(operation, milliseconds = 650) {
  const started = performance.now();
  try {
    return await operation();
  } finally {
    const remaining = milliseconds - (performance.now() - started);
    if (remaining > 0) await new Promise(resolve => window.setTimeout(resolve, remaining));
  }
}

$('#add-server').addEventListener('click', () => showServerDialog());
$('#refresh-servers').addEventListener('click', () => refreshServerStatuses(true));
$('#cancel-server-modal').addEventListener('click', closeServerDialog);
$('#dismiss-server-modal').addEventListener('click', closeServerDialog);
$('#server-modal').addEventListener('click', event => { if (event.target === event.currentTarget) closeServerDialog(); });
$('#save-server').addEventListener('click', async () => {
  const id = $('#server-edit-id').value;
  const nickname = $('#server-nickname').value;
  const host = $('#server-host').value;
  $('#save-server').disabled = true;
  const saved = await updateServerState(
    () => id ? window.launcher.editServer(id, nickname, host) : window.launcher.addServer(nickname, host),
    '#server-modal-error'
  );
  $('#save-server').disabled = false;
  if (saved) {
    closeServerDialog();
    refreshServerStatuses();
  }
});

$('#server-list').addEventListener('click', async event => {
  const button = event.target.closest('button[data-action]');
  if (!button) return;
  const server = state.servers.find(item => item.id === button.dataset.serverId);
  if (!server) return;
  if (button.dataset.action === 'edit') {
    showServerDialog(server);
    return;
  }
  if (button.dataset.action === 'remove') {
    if (!window.confirm(`Remove ${server.nickname} from your server list?`)) return;
    await updateServerState(() => window.launcher.removeServer(server.id));
    return;
  }
  if (button.dataset.action === 'connect') {
    const connecting = server.id !== state.connectedServerId;
    if (connecting) {
      button.disabled = true;
      button.classList.add('connecting');
      button.textContent = 'Connecting';
    }
    const connected = await updateServerState(() => connecting
      ? withMinimumDelay(() => window.launcher.connectServer(server.id))
      : window.launcher.connectServer(null));
    if (!connected) {
      try { render(await window.launcher.getState()); } catch (_) { /* Keep the last known state. */ }
    }
  }
});

function setRegistering(value) {
  registering = value;
  $('#confirm-password-label').classList.toggle('hidden', !value);
  $('#account-password-confirm').classList.toggle('hidden', !value);
  $('#login-account').classList.toggle('hidden', value);
  $('#cancel-register').classList.toggle('hidden', !value);
  $('#register-account').textContent = value ? 'Create account' : 'Register';
  $('#account-password').autocomplete = value ? 'new-password' : 'current-password';
  if (!value) $('#account-password-confirm').value = '';
}

async function runAccountAction(operation) {
  const error = $('#account-error');
  error.classList.add('hidden');
  for (const button of document.querySelectorAll('.account-actions button, #logout-account')) button.disabled = true;
  try {
    render(await operation());
    $('#account-password').value = '';
    $('#account-password-confirm').value = '';
    setRegistering(false);
  } catch (reason) {
    error.textContent = reason?.message || String(reason);
    error.classList.remove('hidden');
  } finally {
    for (const button of document.querySelectorAll('.account-actions button, #logout-account')) button.disabled = false;
  }
}

$('#login-account').addEventListener('click', () => runAccountAction(() => window.launcher.loginAccount($('#account-username').value, $('#account-password').value)));
$('#register-account').addEventListener('click', () => {
  if (!registering) {
    setRegistering(true);
    $('#account-password').focus();
    return;
  }
  if ($('#account-password').value !== $('#account-password-confirm').value) {
    $('#account-error').textContent = 'Passwords do not match.';
    $('#account-error').classList.remove('hidden');
    return;
  }
  runAccountAction(() => window.launcher.registerAccount($('#account-username').value, $('#account-password').value));
});
$('#cancel-register').addEventListener('click', () => setRegistering(false));
$('#logout-account').addEventListener('click', () => runAccountAction(window.launcher.logoutAccount));
$('#account-password').addEventListener('keydown', event => { if (event.key === 'Enter' && !registering) $('#login-account').click(); });
$('#account-password-confirm').addEventListener('keydown', event => { if (event.key === 'Enter' && registering) $('#register-account').click(); });

async function refreshServerStatuses(manual = false) {
  const button = $('#refresh-servers');
  if (serverRefreshBusy) return;
  serverRefreshBusy = true;
  if (manual) {
    button.disabled = true;
    button.textContent = 'Refreshing...';
    $('#server-error').classList.add('hidden');
  }
  try {
    render(await window.launcher.refreshServerStatuses());
  } catch (reason) {
    if (manual) {
      $('#server-error').textContent = reason?.message || String(reason);
      $('#server-error').classList.remove('hidden');
    }
  } finally {
    serverRefreshBusy = false;
    if (manual) {
      button.disabled = false;
      button.textContent = 'Refresh';
    }
  }
}

function openStopModal(process) {
  pendingStopProcess = process;
  stopModalPreviousFocus = document.activeElement;
  const label = process === 'designer' ? 'Offline Designer' : 'OpenShores';
  $('#stop-process-name').textContent = label;
  $('#stop-process-modal').classList.remove('hidden');
  window.setTimeout(() => $('#cancel-stop-process').focus(), 0);
}

function closeStopModal() {
  if ($('#confirm-stop-process').disabled) return;
  $('#stop-process-modal').classList.add('hidden');
  pendingStopProcess = null;
  if (stopModalPreviousFocus?.focus) stopModalPreviousFocus.focus();
}

$('#primary-action').addEventListener('click', () => {
  if (state.gameRunning) openStopModal('game');
  else if (state.installed) launchProcess('game', window.launcher.launch);
  else runOperation(window.launcher.install);
});
$('#offline-designer').addEventListener('click', () => {
  if (state.designerRunning) openStopModal('designer');
  else launchProcess('designer', window.launcher.launchOfflineDesigner);
});
$('#cancel-stop-process').addEventListener('click', closeStopModal);
$('#stop-process-modal').addEventListener('click', event => { if (event.target === event.currentTarget) closeStopModal(); });
$('#confirm-stop-process').addEventListener('click', async () => {
  if (!pendingStopProcess) return;
  const process = pendingStopProcess;
  const button = $('#confirm-stop-process');
  button.disabled = true;
  button.textContent = 'Stopping...';
  clearError();
  try {
    render(await window.launcher.stopProcess(process));
    button.disabled = false;
    closeStopModal();
  } catch (error) {
    showError(error);
  } finally {
    button.disabled = false;
    button.textContent = 'Yes';
  }
});
$('#open-folder').addEventListener('click', () => window.launcher.openFolder().catch(showError));
$('#choose-folder').addEventListener('click', async () => {
  clearError();
  if (state.installed) {
    $('#progress-area').classList.remove('hidden');
    $('#progress-phase').textContent = 'Choose a destination';
    $('#progress-percent').textContent = '0%';
    $('#progress-bar').style.width = '0';
    $('#progress-bar').classList.remove('complete');
    $('#progress-detail').textContent = 'Select an empty folder for the OpenShores installation.';
    switchView('home');
    await runOperation(async () => {
      const next = await window.launcher.chooseFolder();
      if (!next) $('#progress-area').classList.add('hidden');
      return next || state;
    });
    return;
  }
  try {
    const next = await window.launcher.chooseFolder();
    if (next) render(next);
  } catch (error) {
    showError(error);
  }
});
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
document.addEventListener('keydown', event => {
  if (event.key === 'Escape') {
    closeUpdateModal();
    closeServerDialog();
    closeStopModal();
  }
});
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
  refreshServerStatuses();
  serverStatusTimer = window.setInterval(refreshServerStatuses, 10000);
}).catch(error => {
  $('#startup-placeholder').querySelector('strong').textContent = 'Startup could not finish';
  $('#startup-placeholder').querySelector('p').textContent = 'The error below can be selected and copied.';
  $('.startup-spinner').classList.add('hidden');
  $('#status-text').textContent = 'Launcher needs attention';
  showError(error);
});
