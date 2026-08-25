'use strict';

const $ = selector => document.querySelector(selector);
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { platform } = window.__TAURI__.os;
window.launcher = {
  getState: () => invoke('get_state'),
  listLogFiles: () => invoke('list_log_files'),
  setSelectedLogFile: path => invoke('set_selected_log_file', { path }),
  setLogFontSize: size => invoke('set_log_font_size', { size }),
  readClientLog: offset => invoke('read_client_log', { offset }),
  clearClientLog: () => invoke('clear_client_log'),
  chooseFolder: () => invoke('choose_folder'),
  install: () => invoke('install_game'),
  getPatchReleases: () => invoke('get_ip_patch_releases'),
  setPatchRelease: selection => invoke('set_ip_patch_release', { selection }),
  setDeveloperMode: enabled => invoke('set_developer_mode', { enabled }),
  setPostLaunchAction: (process, action) => invoke('set_post_launch_action', { process, action }),
  setTheme: themeId => invoke('set_theme', { themeId }),
  importThemeFile: () => invoke('import_theme_file'),
  importThemeUrl: url => invoke('import_theme_url', { url }),
  deleteTheme: themeId => invoke('delete_theme', { themeId }),
  openThemeEditor: themeId => invoke('open_theme_editor', { themeId }),
  finishThemeElementPicker: selector => invoke('finish_theme_element_picker', { selector }),
  updatePatch: () => invoke('update_ip_patch'),
  uninstall: () => invoke('uninstall_game'),
  addServer: (nickname, host) => invoke('add_server', { nickname, host }),
  editServer: (serverId, nickname, host) => invoke('edit_server', { serverId, nickname, host }),
  removeServer: serverId => invoke('remove_server', { serverId }),
  connectServer: (serverId, force = false) => invoke('connect_server', { serverId, force }),
  refreshServerStatus: serverId => invoke('refresh_server_status', { serverId }),
  refreshServerStatuses: () => invoke('refresh_server_statuses'),
  loginAccount: (username, password) => invoke('login_account', { username, password }),
  registerAccount: (username, password) => invoke('register_account', { username, password }),
  logoutAccount: () => invoke('logout_account'),
  launch: () => invoke('launch_game'),
  launchOfflineDesigner: mode => invoke('launch_offline_designer', { mode }),
  stopProcess: process => invoke('stop_game_process', { process }),
  minimizeLauncher: () => invoke('minimize_launcher'),
  exitLauncher: () => invoke('exit_launcher'),
  openFolder: () => invoke('open_folder'),
  openLink: url => invoke('open_link', { url }),
  checkUpdates: () => invoke('check_updates'),
  checkPatchUpdate: () => invoke('check_ip_patch_update'),
  checkGameUpdate: () => invoke('check_game_update'),
  installUpdate: () => invoke('install_launcher_update'),
  checkPath: (cmd) => invoke('check_path', { cmd }),
  onProgress: callback => listen('operation-progress', event => callback(event.payload)),
  onOperationStatus: callback => listen('operation-status', event => callback(event.payload)),
  onGameStatus: callback => listen('game-status', event => callback(event.payload)),
  onStateChanged: callback => listen('state-changed', event => callback(event.payload)),
  onThemeElementPickerStart: callback => listen('theme-element-picker-start', callback),
  onThemeCssPreview: callback => listen('theme-css-preview', event => callback(event.payload)),
  onThemeCssPreviewClear: callback => listen('theme-css-preview-clear', callback),
  onUpdaterStatus: callback => listen('updater-status', event => callback(event.payload))
};
let state = null;
let busy = false;
let serverStatusTimer = null;
let patchUpdateTimer = null;
let gameUpdateTimer = null;
let logUpdateTimer = null;
let logFilterTimer = null;
let logText = '';
let logLines = [];
let logEndsWithNewline = false;
let filteredLogIndices = null;
let logOffset = 0;
let logLoaded = false;
let logReadBusy = false;
let logActionBusy = false;
let logFilesBusy = false;
let logGeneration = 0;
let logRenderFrame = null;
let logFontSaveTimer = null;
let pendingLogFontSize = null;
const LOG_RENDER_OVERSCAN = 80;
const LOG_FONT_SIZE_MIN = 8;
const LOG_FONT_SIZE_MAX = 24;
let serverRefreshBusy = false;
let registering = false;
const launchingProcesses = new Set();
let pendingStopProcess = null;
let stopModalPreviousFocus = null;
const updateTasks = {
  launcher: { phase: 'idle', message: '' },
  patch: { phase: 'idle', message: '' },
  game: { phase: 'idle', message: '' }
};
const maintenanceQueue = [];
let maintenanceActive = false;
let maintenanceGeneration = 0;
let activeUpdateKind = null;
let latestPatchLabel = 'Latest Release';
let linuxState = {
  winePath: null,
  xdeltaPath: null
}

function connectedServer() {
  return state?.servers?.find(server => server.id === state.connectedServerId) || null;
}
let appliedThemeKey = null;
let appliedThemeUrl = null;
let themePreviewCss = null;

function applyTheme(themeId, css) {
  const key = `${themeId}\0${css}`;
  if (key === appliedThemeKey) return;
  appliedThemeKey = key;
  const oldLink = $('#active-theme-stylesheet');
  if (oldLink) oldLink.remove();
  if (appliedThemeUrl) URL.revokeObjectURL(appliedThemeUrl);
  appliedThemeUrl = null;
  document.documentElement.dataset.theme = themeId || 'default-dark';
  if (!css) return;
  appliedThemeUrl = URL.createObjectURL(new Blob([css], { type: 'text/css' }));
  const link = document.createElement('link');
  link.id = 'active-theme-stylesheet';
  link.rel = 'stylesheet';
  link.href = appliedThemeUrl;
  document.head.appendChild(link);
}

function applyLogFontSize(size) {
  if (size == null) {
    document.documentElement.style.removeProperty('--log-font-size');
    return;
  }
  document.documentElement.style.setProperty('--log-font-size', `${size}px`);
}

function closeThemeMenus() {
  $('#theme-picker').classList.remove('open');
  $('#theme-picker-menu').hidden = true;
  $('#theme-picker-button').setAttribute('aria-expanded', 'false');
  $('#theme-import').classList.remove('open');
  $('#theme-import-menu').hidden = true;
  $('#theme-import-button').setAttribute('aria-expanded', 'false');
}

function renderThemeControls() {
  const themes = state.themes || [];
  const selected = themes.find(theme => theme.id === state.selectedTheme) || themes[0];
  $('#theme-picker-label').textContent = selected?.name || 'Default Dark [SYSTEM]';
  $('#edit-theme').disabled = busy || !selected || selected.builtIn;
  $('#new-theme').disabled = busy;
  $('#theme-import-button').disabled = busy;
  $('#theme-picker-button').disabled = busy;
  const menu = $('#theme-picker-menu');
  menu.replaceChildren();
  for (const theme of themes) {
    const row = document.createElement('div');
    row.className = 'theme-option-row';
    const option = document.createElement('button');
    option.type = 'button';
    option.className = 'theme-option';
    option.setAttribute('role', 'option');
    option.setAttribute('aria-selected', String(theme.id === state.selectedTheme));
    option.textContent = theme.name;
    option.addEventListener('click', () => {
      closeThemeMenus();
      if (theme.id !== state.selectedTheme) runOperation(() => window.launcher.setTheme(theme.id), false);
    });
    row.appendChild(option);
    if (!theme.builtIn) {
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'theme-delete';
      remove.setAttribute('aria-label', `Delete ${theme.name}`);
      remove.title = `Delete ${theme.name}`;
      remove.textContent = '×';
      remove.addEventListener('click', () => {
        closeThemeMenus();
        if (window.confirm(`Delete the “${theme.name}” theme? This cannot be undone.`)) {
          runOperation(() => window.launcher.deleteTheme(theme.id), false);
        }
      });
      row.appendChild(remove);
    }
    menu.appendChild(row);
  }
}

let themeElementPickerActive = false;
let themeElementPickerHover = null;
let themeElementPickerNotice = null;

function escapeCssIdentifier(value) {
  if (window.CSS?.escape) return window.CSS.escape(value);
  return value.replace(/[^a-zA-Z0-9_-]/g, character => `\\${character}`);
}

function selectorForElement(element) {
  if (element.id) return `#${escapeCssIdentifier(element.id)}`;
  const ignoredClasses = new Set([
    'active', 'hidden', 'open', 'loading', 'checking', 'pending', 'updating',
    'process-running', 'theme-element-picker-hover', 'theme-element-picker-active'
  ]);
  const segments = [];
  let current = element;
  while (current && current !== document.documentElement) {
    if (current.id) {
      segments.unshift(`#${escapeCssIdentifier(current.id)}`);
      break;
    }
    let segment = current.tagName.toLowerCase();
    const classes = [...current.classList]
      .filter(className => !ignoredClasses.has(className))
      .slice(0, 3);
    if (classes.length) segment += classes.map(className => `.${escapeCssIdentifier(className)}`).join('');
    const parent = current.parentElement;
    if (parent) {
      const matchingSiblings = [...parent.children].filter(sibling => sibling.matches(segment));
      if (matchingSiblings.length > 1) {
        const sameTag = [...parent.children].filter(sibling => sibling.tagName === current.tagName);
        segment += `:nth-of-type(${sameTag.indexOf(current) + 1})`;
      }
    }
    segments.unshift(segment);
    const candidate = segments.join(' > ');
    try {
      if (document.querySelectorAll(candidate).length === 1) return candidate;
    } catch (_) {
      // Continue toward a uniquely identifiable ancestor.
    }
    current = parent;
  }
  return segments.join(' > ');
}

function clearThemeElementPicker() {
  if (!themeElementPickerActive) return;
  themeElementPickerActive = false;
  themeElementPickerHover?.classList.remove('theme-element-picker-hover');
  themeElementPickerHover = null;
  themeElementPickerNotice?.remove();
  themeElementPickerNotice = null;
  document.documentElement.classList.remove('theme-element-picker-active');
  document.removeEventListener('mouseover', handleThemePickerHover, true);
  document.removeEventListener('click', handleThemePickerClick, true);
  document.removeEventListener('keydown', handleThemePickerKeydown, true);
}

function handleThemePickerHover(event) {
  const target = event.target instanceof Element ? event.target : null;
  if (!target || target === themeElementPickerHover || target === themeElementPickerNotice) return;
  themeElementPickerHover?.classList.remove('theme-element-picker-hover');
  themeElementPickerHover = target;
  target.classList.add('theme-element-picker-hover');
}

function handleThemePickerClick(event) {
  event.preventDefault();
  event.stopImmediatePropagation();
  const target = event.target instanceof Element ? event.target : themeElementPickerHover;
  if (!target) return;
  const selector = selectorForElement(target);
  clearThemeElementPicker();
  window.launcher.finishThemeElementPicker(selector).catch(showError);
}

function handleThemePickerKeydown(event) {
  if (event.key !== 'Escape') return;
  event.preventDefault();
  event.stopImmediatePropagation();
  clearThemeElementPicker();
  window.launcher.finishThemeElementPicker(null).catch(showError);
}

function beginThemeElementPicker() {
  clearThemeElementPicker();
  closeThemeMenus();
  themeElementPickerActive = true;
  document.documentElement.classList.add('theme-element-picker-active');
  themeElementPickerNotice = document.createElement('div');
  themeElementPickerNotice.className = 'theme-element-picker-notice';
  themeElementPickerNotice.textContent = 'Select an element for your theme · Esc to cancel';
  document.body.appendChild(themeElementPickerNotice);
  document.addEventListener('mouseover', handleThemePickerHover, true);
  document.addEventListener('click', handleThemePickerClick, true);
  document.addEventListener('keydown', handleThemePickerKeydown, true);
}

window.launcher.onThemeElementPickerStart(beginThemeElementPicker);

function appendSearchHighlights(target, text, filter) {
  if (!filter) {
    target.append(document.createTextNode(text));
    return;
  }
  const lowerText = text.toLocaleLowerCase();
  const lowerFilter = filter.toLocaleLowerCase();
  let cursor = 0;
  let match = lowerText.indexOf(lowerFilter);
  while (match !== -1) {
    target.append(document.createTextNode(text.slice(cursor, match)));
    const mark = document.createElement('mark');
    mark.textContent = text.slice(match, match + filter.length);
    target.append(mark);
    cursor = match + filter.length;
    match = lowerText.indexOf(lowerFilter, cursor);
  }
  target.append(document.createTextNode(text.slice(cursor)));
}

function appendHighlightedLogLine(target, line, filter) {
  const tokenPattern = /\b(?:FATAL|ERROR|EXCEPTION|WARN(?:ING)?|INFO|DEBUG|TRACE)\b|\b\d{4}-\d{2}-\d{2}[T ][0-9:.+\-Z]+|\b\d{1,2}:\d{2}:\d{2}(?:\.\d+)?\b/gi;
  let cursor = 0;
  for (const match of line.matchAll(tokenPattern)) {
    appendSearchHighlights(target, line.slice(cursor, match.index), filter);
    const token = document.createElement('span');
    const value = match[0];
    const tokenName = /^\d/.test(value) ? 'time' : value.toLowerCase();
    token.className = `log-token-${tokenName}`;
    appendSearchHighlights(token, value, filter);
    target.append(token);
    cursor = match.index + value.length;
  }
  appendSearchHighlights(target, line.slice(cursor), filter);
}

function rebuildLogFilter() {
  const filter = $('#log-filter').value.trim().toLocaleLowerCase();
  filteredLogIndices = filter
    ? logLines.reduce((matches, line, index) => {
        if (line.toLocaleLowerCase().includes(filter)) matches.push(index);
        return matches;
      }, [])
    : null;
}

function setLogText(content, append = false) {
  if (!append) {
    logText = content;
    logLines = content ? content.split(/\r?\n/) : [];
    logEndsWithNewline = /(?:\r?\n)$/.test(content);
    if (logEndsWithNewline) logLines.pop();
  } else if (content) {
    const prefix = logEndsWithNewline ? '' : (logLines.pop() || '');
    logText += content;
    const combined = prefix + content;
    const additions = combined.split(/\r?\n/);
    logEndsWithNewline = /(?:\r?\n)$/.test(combined);
    if (logEndsWithNewline) additions.pop();
    logLines.push(...additions);
  }
  rebuildLogFilter();
}

function createLogRow(lineIndex, filter) {
    const line = logLines[lineIndex];
    const row = document.createElement('div');
    row.className = 'log-line';
    const trimmedLine = line.trimStart();
    if (trimmedLine.startsWith('@@')) row.classList.add('log-diff-section');
    else if (trimmedLine.startsWith('+')) row.classList.add('log-diff-add');
    else if (trimmedLine.startsWith('-')) row.classList.add('log-diff-remove');
    else if (trimmedLine.startsWith('=')) row.classList.add('log-diff-equal');
    else if (/\b(?:fatal|error|exception|failed|failure)\b/i.test(line)) row.classList.add('log-error');
    else if (/\bwarn(?:ing)?\b/i.test(line)) row.classList.add('log-warning');
    else if (/\bdebug\b/i.test(line)) row.classList.add('log-debug');
    else if (/\btrace\b/i.test(line)) row.classList.add('log-trace');
    const number = document.createElement('span');
    number.className = 'log-line-number';
    number.textContent = String(lineIndex + 1);
    const content = document.createElement('span');
    content.className = 'log-line-content';
    appendHighlightedLogLine(content, line, filter);
    row.append(number, content);
    return row;
}

function getLogLineHeight(output = $('#log-output')) {
  const lineHeight = Number.parseFloat(window.getComputedStyle(output).lineHeight);
  return Number.isFinite(lineHeight) && lineHeight > 0 ? lineHeight : 16;
}

function renderLog(scrollToEnd = false) {
  const output = $('#log-output');
  const filter = $('#log-filter').value.trim();
  const visibleCount = filteredLogIndices ? filteredLogIndices.length : logLines.length;
  const lineHeight = getLogLineHeight(output);
  const viewportLines = Math.ceil(output.clientHeight / lineHeight);
  const targetScrollTop = scrollToEnd ? Math.max(0, visibleCount * lineHeight - output.clientHeight) : output.scrollTop;
  const first = Math.max(0, Math.floor(targetScrollTop / lineHeight) - LOG_RENDER_OVERSCAN);
  const last = Math.min(visibleCount, first + viewportLines + LOG_RENDER_OVERSCAN * 2);
  const fragment = document.createDocumentFragment();

  if (visibleCount) {
    const topSpacer = document.createElement('div');
    topSpacer.className = 'log-virtual-spacer';
    topSpacer.style.height = `${first * lineHeight}px`;
    fragment.append(topSpacer);
    for (let position = first; position < last; position += 1) {
      const lineIndex = filteredLogIndices ? filteredLogIndices[position] : position;
      fragment.append(createLogRow(lineIndex, filter));
    }
    const bottomSpacer = document.createElement('div');
    bottomSpacer.className = 'log-virtual-spacer';
    bottomSpacer.style.height = `${(visibleCount - last) * lineHeight}px`;
    fragment.append(bottomSpacer);
  }

  output.replaceChildren();
  if (visibleCount) output.append(fragment);
  else {
    const empty = document.createElement('div');
    empty.className = 'log-empty';
    const selectedName = $('#log-file-select').selectedOptions[0]?.textContent || 'Selected log';
    empty.textContent = filter && logLines.length ? 'No log entries match this filter.' : `${selectedName} is empty.`;
    output.append(empty);
  }
  $('#log-match-count').textContent = filter ? `${visibleCount} of ${logLines.length} lines` : `${logLines.length} lines`;
  $('#copy-log').disabled = !logText;
  $('#clear-log').disabled = logActionBusy;
  if (scrollToEnd) output.scrollTop = output.scrollHeight;
}

async function refreshLogFiles() {
  if (logFilesBusy || !state?.developerMode) return;
  logFilesBusy = true;
  const select = $('#log-file-select');
  const previous = select.value;
  try {
    const files = await window.launcher.listLogFiles();
    const selected = files.find(file => file.selected)?.path || '';
    const fragment = document.createDocumentFragment();
    if (!files.length) {
      const option = document.createElement('option');
      option.value = '';
      option.textContent = 'No .log or .txt files found';
      fragment.append(option);
    } else {
      for (const file of files) {
        const option = document.createElement('option');
        option.value = file.path;
        option.textContent = file.name;
        option.selected = file.path === selected;
        fragment.append(option);
      }
    }
    select.replaceChildren(fragment);
    select.disabled = !files.length;
    if (previous && previous !== select.value) {
      logGeneration += 1;
      logOffset = 0;
      setLogText('');
      renderLog(true);
    }
  } catch (error) {
    const status = $('#log-status');
    status.textContent = 'Log scan failed';
    status.title = error?.message || String(error);
  } finally {
    logFilesBusy = false;
  }
}

async function readClientLog(reset = false) {
  if (logReadBusy || logActionBusy || !state?.developerMode) return;
  logReadBusy = true;
  const generation = logGeneration;
  const output = $('#log-output');
  const wasAtBottom = output.scrollHeight - output.scrollTop - output.clientHeight < 30;
  const requestedOffset = reset ? 0 : logOffset;
  try {
    const chunk = await window.launcher.readClientLog(requestedOffset);
    if (generation !== logGeneration) return;
    const changed = chunk.reset || requestedOffset === 0 || !!chunk.content;
    setLogText(chunk.content, !(chunk.reset || requestedOffset === 0));
    logOffset = chunk.nextOffset;
    logLoaded = true;
    const status = $('#log-status');
    const watchingFile = $('#log-view').classList.contains('active');
    status.textContent = state.gameRunning ? 'Watching live' : watchingFile ? 'Watching file' : 'Game is not running';
    status.classList.toggle('live', !!state.gameRunning || watchingFile);
    status.removeAttribute('title');
    if (changed && $('#log-view').classList.contains('active')) renderLog(reset || wasAtBottom);
    if (chunk.hasMore) window.setTimeout(() => readClientLog(), 0);
  } catch (error) {
    const status = $('#log-status');
    status.textContent = 'Log unavailable';
    status.classList.remove('live');
    status.title = error?.message || String(error);
  } finally {
    logReadBusy = false;
  }
}

function syncLogMonitoring() {
  const status = $('#log-status');
  const logViewActive = $('#log-view').classList.contains('active');
  const shouldMonitor = !!state?.developerMode && (!!state?.gameRunning || logViewActive);
  status.textContent = state?.gameRunning ? 'Watching live' : logViewActive ? 'Watching file' : 'Game is not running';
  status.classList.toggle('live', shouldMonitor);
  status.removeAttribute('title');
  if (shouldMonitor && !logUpdateTimer) {
    readClientLog(true);
    logUpdateTimer = window.setInterval(() => readClientLog(), 500);
  } else if (!shouldMonitor && logUpdateTimer) {
    window.clearInterval(logUpdateTimer);
    logUpdateTimer = null;
    if (state?.developerMode) readClientLog();
  }
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
    const developerConnect = state.developerMode && server.id !== state.connectedServerId && !online;
    connect.disabled = server.id !== state.connectedServerId && !online && !developerConnect;
    connect.classList.toggle('developer-force-connect', developerConnect);
    if (developerConnect) {
      connect.dataset.forceConnect = 'true';
      connect.setAttribute('aria-disabled', 'true');
      connect.title = 'Unavailable. Double-click to connect in Developer mode.';
    } else {
      connect.title = connect.disabled ? 'This server must be online before you can connect.' : '';
    }
    const controls = document.createElement('div');
    controls.className = 'server-card-controls';
    const tools = document.createElement('div');
    tools.className = 'server-card-tools';
    const refresh = document.createElement('button');
    refresh.className = 'icon-button';
    refresh.dataset.action = 'refresh';
    refresh.dataset.serverId = server.id;
    refresh.setAttribute('aria-label', `Refresh ${server.nickname}`);
    refresh.title = 'Refresh server status';
    refresh.textContent = 'Refresh';
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
    tools.append(refresh, edit, remove);
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

const updateTaskUi = {
  launcher: { button: '#check-updates', idle: 'Check now' },
  patch: { button: '#update-patch', idle: 'Update patch' },
  game: { button: '#refresh-game', idle: 'Update game' }
};

function renderUpdateTask(kind) {
  const task = updateTasks[kind];
  const ui = updateTaskUi[kind];
  const button = $(ui.button);
  if (!button) return;
  const labels = { pending: 'Pending...', checking: 'Checking...', updating: kind === 'launcher' ? 'Restarting...' : 'Updating...' };
  button.querySelector('.button-label').textContent = labels[task.phase] || ui.idle;
  button.classList.toggle('pending', task.phase === 'pending');
  button.classList.toggle('checking', task.phase === 'checking');
  button.classList.toggle('updating', task.phase === 'updating');
  const unavailable = kind !== 'launcher' && !state?.installed;
  const processBlocked = kind !== 'launcher' && (state?.gameRunning || state?.designerRunning);
  const waiting = ['pending', 'checking', 'updating'].includes(task.phase);
  button.disabled = unavailable || processBlocked || waiting || (busy && task.phase !== 'updating');
  const wrapper = button.closest('.update-button-wrap');
  if (task.phase === 'pending') wrapper.dataset.tooltip = 'Pending...';
  else if (task.phase === 'checking') wrapper.dataset.tooltip = task.message || `Checking ${kind === 'launcher' ? 'the launcher' : kind === 'patch' ? 'the IP patch' : 'game files'} for updates.`;
  else if (task.phase === 'updating') wrapper.dataset.tooltip = task.message || 'This update is currently in progress.';
  else if (task.phase === 'error') wrapper.dataset.tooltip = task.message || 'The update task failed. Click to try again.';
  else if (unavailable) wrapper.dataset.tooltip = 'Install OpenShores first.';
  else if (processBlocked) wrapper.dataset.tooltip = 'Close OpenShores and Offline Designer first.';
  else if (busy) wrapper.dataset.tooltip = 'Another launcher operation is currently running.';
  else wrapper.dataset.tooltip = '';
}

function renderUpdateTasks() {
  Object.keys(updateTasks).forEach(renderUpdateTask);
}

function removeUpdateBanner(kind) {
  document.querySelector(`.update-banner[data-kind="${kind}"]`)?.remove();
}

function showUpdateBanner(kind, message, error = false) {
  removeUpdateBanner(kind);
  const banner = document.createElement('section');
  banner.className = `update-banner ${kind}${error ? ' error' : ''}`;
  banner.dataset.kind = kind;
  const icon = document.createElement('span');
  icon.className = 'update-banner-icon';
  icon.textContent = error ? '!' : '\u2193';
  const copy = document.createElement('div');
  copy.className = 'update-banner-copy';
  const title = document.createElement('strong');
  title.textContent = error ? 'Update check failed' : kind === 'launcher' ? 'Launcher update available' : kind === 'patch' ? 'IP patch update available' : 'Game client update available';
  const detail = document.createElement('span');
  detail.textContent = message;
  copy.append(title, detail);
  banner.append(icon, copy);
  if (!error) {
    const action = document.createElement('button');
    action.className = 'primary';
    action.textContent = kind === 'launcher' ? 'Download & restart' : kind === 'patch' ? 'Update patch' : 'Update game';
    action.addEventListener('click', () => kind === 'launcher' ? installLauncherUpdateNow() : enqueueMaintenance(kind, 'update'));
    banner.append(action);
  }
  const dismiss = document.createElement('button');
  dismiss.className = 'banner-dismiss';
  dismiss.setAttribute('aria-label', 'Dismiss update notification');
  dismiss.textContent = '\u00d7';
  dismiss.addEventListener('click', () => banner.remove());
  banner.append(dismiss);
  $('#update-banners').append(banner);
}

function setUpdateTask(kind, phase, message = '') {
  updateTasks[kind] = { ...updateTasks[kind], phase, message };
  if (kind === 'launcher') setUpdateCheckResult(message, phase === 'checking' ? 'checking' : phase === 'current' ? 'success' : phase === 'error' ? 'error' : '');
  renderUpdateTask(kind);
}

function renderProcessButton(button, process, running, idleLabel, enabled) {
  const launching = launchingProcesses.has(process);
  button.classList.toggle('process-running', running);
  const label = button.querySelector('.process-button-label');
  const text = running ? 'Running...' : launching ? 'Launching...' : idleLabel;
  if (label) label.textContent = text;
  else button.textContent = text;
  button.disabled = launching || (!running && (!enabled || busy));
}

function closeDesignerMenu() {
  const control = $('#offline-designer-control');
  const button = $('#offline-designer');
  control.classList.remove('open');
  $('#offline-designer-menu').hidden = true;
  button.setAttribute('aria-expanded', 'false');
}

function openDesignerMenu() {
  const control = $('#offline-designer-control');
  const button = $('#offline-designer');
  control.classList.add('open');
  $('#offline-designer-menu').hidden = false;
  button.setAttribute('aria-expanded', 'true');
  $('#offline-designer-menu [role="menuitem"]').focus();
}

function render(nextState = state) {
  if (!nextState) return;
  state = nextState;
  applyTheme(themePreviewCss === null ? state.selectedTheme : 'theme-preview', themePreviewCss ?? state.themeCss ?? '');
  applyLogFontSize(pendingLogFontSize ?? state.logFontSize);
  $('.version').textContent = `v${state.launcherVersion}`;
  $('#section-nav').classList.remove('hidden');
  $('#startup-placeholder').classList.add('hidden');
  $('#actions').classList.remove('hidden');
  $('#install-path').textContent = state.installPath;
  $('#patch-channel').textContent = state.ipPatchRelease === 'latest'
    ? latestPatchLabel
    : state.ipPatchRelease === 'none' ? 'Disable IP Patch' : state.ipPatchRelease;
  $('#patch-release').value = state.ipPatchRelease || 'latest';
  $('#patch-release').classList.toggle('disabled-selection', state.ipPatchRelease === 'none');
  $('#patch-badge').classList.toggle('hidden', !state.installed);
  $('#game-source').textContent = connectedServer()?.host || 'No server selected';
  const anyProcessRunning = state.gameRunning || state.designerRunning;
  $('#open-folder').disabled = !state.installed || busy;
  $('#uninstall').disabled = !state.installed || busy;
  $('#patch-release').disabled = busy;
  $('#patch-release').classList.toggle('hidden', !state.developerMode);
  $('#refresh-patch-releases').classList.toggle('hidden', !state.developerMode);
  $('#log-tab').classList.toggle('hidden', !state.developerMode);
  if (!state.developerMode && $('#log-tab').classList.contains('active')) switchView('home');
  $('#developer-mode').checked = !!state.developerMode;
  $('#developer-mode').disabled = busy;
  $('#game-launch-action').value = state.gameLaunchAction || 'do_nothing';
  $('#designer-launch-action').value = state.designerLaunchAction || 'do_nothing';
  $('#game-launch-action').disabled = busy;
  $('#designer-launch-action').disabled = busy;
  $('#game-launch-open-logs').hidden = !state.developerMode;
  renderThemeControls();
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
  renderProcessButton(
    $('#log-launch'),
    'game',
    !!state.gameRunning,
    state.installed ? 'Launch OpenShores' : 'Install OpenShores',
    true
  );
  renderProcessButton(designer, 'designer', !!state.designerRunning, 'Offline Designers...', state.installed);
  if (state.designerRunning || launchingProcesses.has('designer') || designer.disabled) closeDesignerMenu();
  renderUpdateTasks();
  syncLogMonitoring();
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
    pumpMaintenanceQueue();
  }
}

async function launchProcess(process, operation) {
  if (launchingProcesses.has(process)) return;
  launchingProcesses.add(process);
  clearError();
  render();
  try {
    state = { ...state, ...(await operation()) };
    try {
      await applyPostLaunchAction(process);
    } catch (error) {
      showError(`The process started, but the selected launcher action failed: ${error?.message || String(error)}`);
    }
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
  if (activeUpdateKind) {
    $(updateTaskUi[activeUpdateKind].button).style.setProperty('--task-progress', `${data.percent}%`);
    updateTasks[activeUpdateKind].message = `${data.phase}, ${data.percent}%`;
    renderUpdateTask(activeUpdateKind);
  }
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
  if (!busy) pumpMaintenanceQueue();
});

window.launcher.onGameStatus(data => {
  if (data.process === 'designer') state.designerRunning = data.running;
  else state.gameRunning = data.running;
  if (data.error) showError(data.error);
  render();
});

window.launcher.onStateChanged(render);
window.launcher.onThemeCssPreview(css => {
  themePreviewCss = css;
  applyTheme('theme-preview', css);
});
window.launcher.onThemeCssPreviewClear(() => {
  themePreviewCss = null;
  if (state) applyTheme(state.selectedTheme, state.themeCss || '');
});

function handleUpdaterStatus(data) {
  if (data.state === 'available') {
    setUpdateTask('launcher', 'available', data.message);
    showUpdateBanner('launcher', data.message);
  } else if (data.state === 'downloading' || data.state === 'installing') {
    setUpdateTask('launcher', 'updating', data.message);
    const percent = data.message.match(/(\d+)%/)?.[1];
    if (percent) $('#check-updates').style.setProperty('--task-progress', `${percent}%`);
    let banner = document.querySelector('.update-banner[data-kind="launcher"]');
    if (!banner) {
      showUpdateBanner('launcher', data.message);
      banner = document.querySelector('.update-banner[data-kind="launcher"]');
    }
    banner.querySelector('.update-banner-copy span').textContent = data.message;
    const action = banner.querySelector('.primary');
    if (action) {
      action.disabled = true;
      action.textContent = data.state === 'downloading' ? 'Downloading...' : 'Restarting...';
    }
  } else if (data.state === 'current') {
    setUpdateTask('launcher', 'current', data.message);
    removeUpdateBanner('launcher');
  } else if (data.state === 'error') {
    setUpdateTask('launcher', 'error', data.message);
    showUpdateBanner('launcher', data.message, true);
  }
}

window.launcher.onUpdaterStatus(handleUpdaterStatus);

async function runMaintenanceItem(item) {
  const generation = item.generation;
  activeUpdateKind = item.action === 'update' ? item.kind : null;
  setUpdateTask(item.kind, item.action === 'check' ? 'checking' : 'updating', item.action === 'check' ? `Checking ${item.kind} updates...` : `Updating ${item.kind}...`);
  try {
    if (item.action === 'check') {
      const check = item.kind === 'launcher' ? window.launcher.checkUpdates
        : item.kind === 'patch' ? window.launcher.checkPatchUpdate
          : window.launcher.checkGameUpdate;
      const result = await check();
      if (generation !== maintenanceGeneration) return;
      if (item.kind === 'launcher') {
        handleUpdaterStatus(result);
      } else {
        setUpdateTask(item.kind, result.state, result.message);
        if (result.state === 'available') showUpdateBanner(item.kind, result.message);
        else removeUpdateBanner(item.kind);
      }
      return;
    }

    clearError();
    $('#progress-area').classList.remove('hidden');
    busy = true;
    render();
    const operation = item.kind === 'patch' ? window.launcher.updatePatch : window.launcher.install;
    state = { ...state, ...(await operation()) };
    removeUpdateBanner(item.kind);
    if (item.kind === 'game') {
      removeUpdateBanner('patch');
      setUpdateTask(
        'patch',
        'current',
        state.ipPatchRelease === 'none'
          ? 'The IP patch remains disabled after the game update.'
          : 'The selected IP patch was reapplied with the game update.'
      );
    }
    setUpdateTask(
      item.kind,
      'current',
      item.kind === 'patch'
        ? state.ipPatchRelease === 'none' ? 'The IP patch is disabled.' : 'IP patch is up to date.'
        : 'OpenShores game files are up to date.'
    );
  } catch (error) {
    const message = error?.message || String(error);
    setUpdateTask(item.kind, 'error', message);
    if (item.action === 'update') showError(message);
  } finally {
    if (item.action === 'update') {
      busy = false;
      activeUpdateKind = null;
      $(updateTaskUi[item.kind].button).style.removeProperty('--task-progress');
      try { state = { ...state, ...(await window.launcher.getState()) }; } catch (_) { /* Keep the last known state. */ }
      render();
    }
  }
}

async function pumpMaintenanceQueue() {
  if (maintenanceActive || busy) return;
  maintenanceActive = true;
  while (maintenanceQueue.length && !busy) {
    const item = maintenanceQueue.shift();
    if (item.generation === maintenanceGeneration) await runMaintenanceItem(item);
  }
  maintenanceActive = false;
}

function enqueueMaintenance(kind, action = 'check') {
  if (action === 'update') {
    for (let index = maintenanceQueue.length - 1; index >= 0; index -= 1) {
      if (maintenanceQueue[index].kind === kind) maintenanceQueue.splice(index, 1);
    }
  } else if (maintenanceQueue.some(item => item.kind === kind && item.action === action) || ['pending', 'checking', 'updating'].includes(updateTasks[kind].phase)) {
    return;
  }
  maintenanceQueue.push({ kind, action, generation: maintenanceGeneration });
  setUpdateTask(kind, 'pending', 'Waiting for earlier update checks to finish.');
  pumpMaintenanceQueue();
}

async function installLauncherUpdateNow() {
  maintenanceGeneration += 1;
  maintenanceQueue.splice(0);
  for (const kind of ['patch', 'game']) {
    if (['pending', 'checking'].includes(updateTasks[kind].phase)) setUpdateTask(kind, 'idle');
  }
  setUpdateTask('launcher', 'updating', 'Starting launcher update...');
  activeUpdateKind = 'launcher';
  try {
    await window.launcher.installUpdate();
  } catch (error) {
    const message = error?.message || String(error);
    handleUpdaterStatus({ state: 'error', message });
    activeUpdateKind = null;
  }
}

function switchView(viewName) {
  const button = document.querySelector(`.nav-item[data-view="${viewName}"]`);
  if (!button || button.disabled || button.classList.contains('hidden')) return;
  document.querySelectorAll('.nav-item').forEach(item => item.classList.toggle('active', item === button));
  document.querySelectorAll('.view').forEach(view => view.classList.remove('active'));
  $(`#${viewName}-view`).classList.add('active');
  if (viewName === 'log') {
    renderLog(true);
    refreshLogFiles().then(() => readClientLog(true));
  }
  syncLogMonitoring();
}

async function applyPostLaunchAction(process) {
  const action = process === 'designer' ? state.designerLaunchAction : state.gameLaunchAction;
  if (action === 'open_logs' && state.developerMode) switchView('log');
  else if (action === 'minimize') await window.launcher.minimizeLauncher();
  else if (action === 'exit') await window.launcher.exitLauncher();
}

document.querySelectorAll('.nav-item').forEach(button => button.addEventListener('click', () => {
  switchView(button.dataset.view);
  if (button.dataset.view === 'settings') {
    window.launcher.getState().then(render).catch(showError);
  }
}));

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

async function connectFromServerButton(button, server, force = false) {
  const connecting = server.id !== state.connectedServerId;
  if (connecting) {
    button.disabled = true;
    button.classList.add('connecting');
    button.textContent = 'Connecting';
  }
  const connected = await updateServerState(() => connecting
    ? withMinimumDelay(() => window.launcher.connectServer(server.id, force))
    : window.launcher.connectServer(null));
  if (!connected) {
    try { render(await window.launcher.getState()); } catch (_) { /* Keep the last known state. */ }
  }
}

$('#server-list').addEventListener('click', async event => {
  const button = event.target.closest('button[data-action]');
  if (!button) return;
  const server = state.servers.find(item => item.id === button.dataset.serverId);
  if (!server) return;
  if (button.dataset.action === 'refresh') {
    button.disabled = true;
    button.textContent = 'Refreshing...';
    await updateServerState(() => window.launcher.refreshServerStatus(server.id));
    return;
  }
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
    if (button.dataset.forceConnect === 'true') return;
    await connectFromServerButton(button, server);
  }
});

$('#server-list').addEventListener('dblclick', async event => {
  const button = event.target.closest('button[data-force-connect="true"]');
  if (!button || !state.developerMode) return;
  const server = state.servers.find(item => item.id === button.dataset.serverId);
  if (!server) return;
  event.preventDefault();
  await connectFromServerButton(button, server, true);
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

function handleGameAction() {
  if (state.gameRunning) openStopModal('game');
  else if (state.installed) launchProcess('game', window.launcher.launch);
  else runOperation(window.launcher.install);
}

$('#primary-action').addEventListener('click', handleGameAction);
$('#log-launch').addEventListener('click', handleGameAction);
$('#offline-designer').addEventListener('click', () => {
  if (state.designerRunning) openStopModal('designer');
  else if ($('#offline-designer-menu').hidden) openDesignerMenu();
  else closeDesignerMenu();
});
$('#offline-designer-menu').addEventListener('click', event => {
  const option = event.target.closest('[data-designer-mode]');
  if (!option) return;
  const mode = option.dataset.designerMode;
  closeDesignerMenu();
  launchProcess('designer', () => window.launcher.launchOfflineDesigner(mode));
});
document.addEventListener('click', event => {
  if (!event.target.closest('#offline-designer-control')) closeDesignerMenu();
});
document.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || $('#offline-designer-menu').hidden) return;
  closeDesignerMenu();
  $('#offline-designer').focus();
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
$('#refresh-game').addEventListener('click', () => enqueueMaintenance('game', 'update'));
$('#update-patch').addEventListener('click', () => enqueueMaintenance('patch', 'update'));
$('#refresh-patch-releases').addEventListener('click', () => {
  clearError();
  loadPatchReleases();
});
$('#patch-release').addEventListener('change', event => {
  const selection = event.currentTarget.value;
  state = { ...state, ipPatchRelease: selection };
  runOperation(async () => {
    const next = await window.launcher.setPatchRelease(selection);
    enqueueMaintenance('patch', 'check');
    return next;
  }, false);
});
$('#check-updates').addEventListener('click', () => enqueueMaintenance('launcher', 'check'));
$('#log-filter').addEventListener('input', () => {
  window.clearTimeout(logFilterTimer);
  logFilterTimer = window.setTimeout(() => {
    $('#log-output').scrollTop = 0;
    rebuildLogFilter();
    renderLog();
  }, 100);
});
$('#log-output').addEventListener('scroll', () => {
  if (logRenderFrame !== null) return;
  logRenderFrame = window.requestAnimationFrame(() => {
    logRenderFrame = null;
    renderLog();
  });
});
$('#log-output').addEventListener('wheel', event => {
  if (!event.ctrlKey || event.deltaY === 0) return;
  event.preventDefault();
  const output = event.currentTarget;
  const oldLineHeight = getLogLineHeight(output);
  const wasAtBottom = output.scrollHeight - output.scrollTop - output.clientHeight < 30;
  const anchorLine = output.scrollTop / oldLineHeight;
  const currentSize = Number.parseFloat(window.getComputedStyle(output).fontSize);
  const direction = event.deltaY < 0 ? 1 : -1;
  if ((direction > 0 && currentSize >= LOG_FONT_SIZE_MAX) || (direction < 0 && currentSize <= LOG_FONT_SIZE_MIN)) return;
  const steppedSize = direction > 0 ? Math.floor(currentSize) + 1 : Math.ceil(currentSize) - 1;
  const nextSize = Math.min(LOG_FONT_SIZE_MAX, Math.max(LOG_FONT_SIZE_MIN, steppedSize));
  pendingLogFontSize = nextSize;
  applyLogFontSize(nextSize);
  if (!wasAtBottom) output.scrollTop = anchorLine * getLogLineHeight(output);
  renderLog(wasAtBottom);
  window.clearTimeout(logFontSaveTimer);
  logFontSaveTimer = window.setTimeout(async () => {
    const sizeToSave = pendingLogFontSize;
    try {
      await window.launcher.setLogFontSize(sizeToSave);
      if (pendingLogFontSize === sizeToSave) {
        state = { ...state, logFontSize: sizeToSave };
        pendingLogFontSize = null;
      }
    } catch (error) {
      if (pendingLogFontSize === sizeToSave) {
        pendingLogFontSize = null;
        applyLogFontSize(state?.logFontSize);
        renderLog(wasAtBottom);
      }
      showError(error);
    }
  }, 180);
}, { passive: false });
$('#log-file-select').addEventListener('pointerdown', () => refreshLogFiles());
$('#log-file-select').addEventListener('keydown', event => {
  if (event.key === 'Enter' || event.key === ' ' || (event.altKey && event.key === 'ArrowDown')) refreshLogFiles();
});
$('#log-file-select').addEventListener('change', async event => {
  const selected = event.currentTarget.value;
  if (!selected || logActionBusy) return;
  logGeneration += 1;
  logOffset = 0;
  setLogText('');
  renderLog(true);
  try {
    await window.launcher.setSelectedLogFile(selected);
    await readClientLog(true);
  } catch (error) {
    const status = $('#log-status');
    status.textContent = 'Could not select log';
    status.classList.remove('live');
    status.title = error?.message || String(error);
    await refreshLogFiles();
  }
});
$('#copy-log').addEventListener('click', async () => {
  if (!logText) return;
  const button = $('#copy-log');
  const copiedLog = `\`\`\`diff\n${logText}${logText.endsWith('\n') ? '' : '\n'}\`\`\``;
  try {
    await navigator.clipboard.writeText(copiedLog);
  } catch (_) {
    const textarea = document.createElement('textarea');
    textarea.value = copiedLog;
    textarea.style.position = 'fixed';
    textarea.style.opacity = '0';
    document.body.append(textarea);
    textarea.select();
    document.execCommand('copy');
    textarea.remove();
  }
  button.textContent = 'Copied';
  window.setTimeout(() => { button.textContent = 'Copy whole log'; }, 1200);
});
$('#clear-log').addEventListener('click', async () => {
  if (logActionBusy) return;
  logActionBusy = true;
  logGeneration += 1;
  renderLog();
  const button = $('#clear-log');
  button.textContent = 'Clearing…';
  try {
    await window.launcher.clearClientLog();
    setLogText('');
    logOffset = 0;
    logLoaded = true;
    renderLog();
  } catch (error) {
    const status = $('#log-status');
    status.textContent = 'Could not clear log';
    status.classList.remove('live');
    status.title = error?.message || String(error);
  } finally {
    logActionBusy = false;
    button.textContent = 'Clear Log';
    renderLog();
  }
});
$('#developer-mode').addEventListener('change', event => {
  const enabled = event.currentTarget.checked;
  state = { ...state, developerMode: enabled };
  runOperation(() => window.launcher.setDeveloperMode(enabled), false);
});
for (const [selector, process] of [['#game-launch-action', 'game'], ['#designer-launch-action', 'designer']]) {
  $(selector).addEventListener('change', event => {
    const action = event.currentTarget.value;
    state = { ...state, [process === 'game' ? 'gameLaunchAction' : 'designerLaunchAction']: action };
    runOperation(() => window.launcher.setPostLaunchAction(process, action), false);
  });
}
$('#theme-picker-button').addEventListener('click', event => {
  event.stopPropagation();
  const picker = $('#theme-picker');
  const opening = !picker.classList.contains('open');
  closeThemeMenus();
  if (opening) {
    picker.classList.add('open');
    $('#theme-picker-menu').hidden = false;
    $('#theme-picker-button').setAttribute('aria-expanded', 'true');
    $('#theme-picker-menu .theme-option')?.focus();
  }
});
$('#theme-import-button').addEventListener('click', event => {
  event.stopPropagation();
  const importer = $('#theme-import');
  const opening = !importer.classList.contains('open');
  closeThemeMenus();
  if (opening) {
    importer.classList.add('open');
    $('#theme-import-menu').hidden = false;
    $('#theme-import-button').setAttribute('aria-expanded', 'true');
    $('#import-theme-file').focus();
  }
});
$('#import-theme-file').addEventListener('click', () => {
  closeThemeMenus();
  runOperation(() => window.launcher.importThemeFile(), false);
});
$('#import-theme-url').addEventListener('click', () => {
  closeThemeMenus();
  const url = window.prompt('Enter the URL of a CSS theme:');
  if (url?.trim()) runOperation(() => window.launcher.importThemeUrl(url.trim()), false);
});
$('#new-theme').addEventListener('click', () => window.launcher.openThemeEditor(null).catch(showError));
$('#edit-theme').addEventListener('click', () => {
  const selected = state?.themes?.find(theme => theme.id === state.selectedTheme);
  if (selected && !selected.builtIn) window.launcher.openThemeEditor(selected.id).catch(showError);
});
document.addEventListener('click', event => {
  if (!event.target.closest('.theme-picker, .theme-import')) closeThemeMenus();
});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape') {
    closeThemeMenus();
    closeServerDialog();
    closeStopModal();
  }
});
document.querySelectorAll('[data-url]').forEach(button => button.addEventListener('click', () => window.launcher.openLink(button.dataset.url).catch(showError)));

const creatorCredit = $('.creator-credit');
let creatorHeartsUnlocked = false;

function releaseCreatorHearts(count) {
  const creditBounds = creatorCredit.getBoundingClientRect();
  const colors = ['#ff769d', '#ff9fba', '#f36f92', '#ffc0d1', '#e885ad'];

  for (let index = 0; index < count; index += 1) {
    const heart = document.createElement('span');
    const duration = 1700 + Math.random() * 1100;
    heart.className = 'creator-heart';
    heart.textContent = '\u2665';
    heart.setAttribute('aria-hidden', 'true');
    heart.style.left = `${creditBounds.left + Math.random() * creditBounds.width}px`;
    heart.style.top = `${creditBounds.top + creditBounds.height * .25}px`;
    heart.style.setProperty('--heart-color', colors[Math.floor(Math.random() * colors.length)]);
    heart.style.setProperty('--heart-size', `${7 + Math.random() * 7}px`);
    heart.style.setProperty('--heart-drift', `${-34 + Math.random() * 68}px`);
    heart.style.setProperty('--heart-rise', `${-55 - Math.random() * 55}px`);
    heart.style.setProperty('--heart-turn', `${-25 + Math.random() * 50}deg`);
    heart.style.setProperty('--heart-duration', `${duration}ms`);
    heart.style.animationDelay = `${Math.random() * 220}ms`;
    document.body.appendChild(heart);
    heart.addEventListener('animationend', () => heart.remove(), { once: true });
  }
}

creatorCredit.addEventListener('dblclick', () => {
  creatorHeartsUnlocked = true;
  releaseCreatorHearts(8);
});
creatorCredit.addEventListener('click', () => {
  if (creatorHeartsUnlocked) releaseCreatorHearts(3);
});

async function loadPatchReleases() {
  const select = $('#patch-release');
  const refresh = $('#refresh-patch-releases');
  refresh.disabled = true;
  refresh.classList.add('loading');
  try {
    const releases = await window.launcher.getPatchReleases();
    const selected = state?.ipPatchRelease || 'latest';
    const latest = releases.find(release => !release.prerelease);
    latestPatchLabel = latest ? `Latest Release (${latest.name})` : 'Latest Release';
    select.replaceChildren(new Option(latestPatchLabel, 'latest'));
    for (const release of releases) {
      const date = release.publishedAt
        ? new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' }).format(new Date(release.publishedAt))
        : 'Unknown date';
      const option = new Option(`${release.name} (${release.tag}) — ${date}`, release.tag);
      option.disabled = !release.hasZip;
      if (!release.hasZip) option.textContent += ' — no ZIP asset';
      select.add(option);
    }
    const disabledOption = new Option('Disable IP Patch', 'none');
    disabledOption.className = 'patch-option-disabled';
    select.add(disabledOption);
    select.value = selected;
    if (state) render();
  } catch (error) {
    showError(`Could not load IP patch releases: ${error?.message || String(error)}`);
  } finally {
    select.disabled = busy;
    refresh.disabled = busy;
    refresh.classList.remove('loading');
  }
}

async function loadLinuxState() {
  linuxState = {
    winePath: null,
    xdeltaPath: null
  }

  try {
    linuxState.winePath = (await window.launcher.checkPath("wine")).path;
  } catch (error) {
    showError(`wine - Missing from $PATH: ${error?.message || String(error)}`);
  }

  try {
    linuxState.xdeltaPath = (await window.launcher.checkPath("xdelta3")).path;
  } catch (error) {
    showError(`xdelta3 - Missing from $PATH: ${error?.message || String(error)}`)
  }

  try {
    const container = $('#linux-info');
    container.innerHTML = '';

    for (const [title, value] of [['WINE PATH', linuxState.winePath], ['XDELTA3 PATH', linuxState.xdeltaPath]]) {
      if (value == null) {
        continue;
      }
      
      const outer = document.createElement("div");
      const span = document.createElement("span");
      span.textContent = title;
      const strong = document.createElement("strong");
      strong.textContent = value;

      outer.appendChild(span);
      outer.appendChild(strong);
      container.appendChild(outer);
    }
  } catch (error){
    console.log(error);
    showError(`Failed to update linux-specific state: ${error?.message}`);
  }
}

window.launcher.getState().then(async nextState => {
  render(nextState);
  if (platform() == "linux") {
    loadLinuxState();
  }
  refreshServerStatuses();
  serverStatusTimer = window.setInterval(refreshServerStatuses, 10000);
  await loadPatchReleases();
  enqueueMaintenance('launcher', 'check');
  enqueueMaintenance('patch', 'check');
  enqueueMaintenance('game', 'check');
  patchUpdateTimer = window.setInterval(() => enqueueMaintenance('patch', 'check'), 6 * 60 * 60 * 1000);
  gameUpdateTimer = window.setInterval(() => enqueueMaintenance('game', 'check'), 12 * 60 * 60 * 1000);
}).catch(error => {
  $('#startup-placeholder').querySelector('strong').textContent = 'Startup could not finish';
  $('#startup-placeholder').querySelector('p').textContent = 'The error below can be selected and copied.';
  $('.startup-spinner').classList.add('hidden');
  $('#status-text').textContent = 'Launcher needs attention';
  showError(error);
});
