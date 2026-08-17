'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const themeId = new URLSearchParams(window.location.search).get('theme');
let originalId = themeId;
let saving = false;
let selectingElement = false;
let previewTimer = null;

const nameInput = document.querySelector('#theme-name');
const cssEditor = document.querySelector('#theme-css');
const highlightCode = document.querySelector('#theme-highlight code');
const highlightLayer = document.querySelector('#theme-highlight');
const lineNumberContent = document.querySelector('#line-number-content');
const swatchLayer = document.querySelector('#color-swatches');
const status = document.querySelector('#editor-status');
const saveButton = document.querySelector('#save-theme');
const selectElementButton = document.querySelector('#select-element');
let colorPopover = null;
const cssColorPattern = /#[0-9a-fA-F]{3,8}\b|rgba?\(\s*\d{1,3}(?:\.\d+)?\s*,\s*\d{1,3}(?:\.\d+)?\s*,\s*\d{1,3}(?:\.\d+)?(?:\s*,\s*(?:0|1|0?\.\d+))?\s*\)/gi;

function escapeHtml(text) {
  return text
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;');
}

function colorDecorationHtml(token, start, syntaxClass = 'color') {
  return `<span class="syntax-color-decoration" data-start="${start}" data-end="${start + token.length}" data-color="${normalizedPickerColor(token)}"><span class="syntax-${syntaxClass}">${escapeHtml(token)}</span></span>`;
}

function highlightedComment(comment, start) {
  let output = '';
  let cursor = 0;
  for (const match of comment.matchAll(cssColorPattern)) {
    output += `<span class="syntax-comment">${escapeHtml(comment.slice(cursor, match.index))}</span>`;
    output += colorDecorationHtml(match[0], start + match.index, 'comment');
    cursor = match.index + match[0].length;
  }
  output += `<span class="syntax-comment">${escapeHtml(comment.slice(cursor))}</span>`;
  return output;
}

function highlightCss(css) {
  const tokenPattern = /\/\*[\s\S]*?\*\/|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|rgba?\(\s*\d{1,3}(?:\.\d+)?\s*,\s*\d{1,3}(?:\.\d+)?\s*,\s*\d{1,3}(?:\.\d+)?(?:\s*,\s*(?:0|1|0?\.\d+))?\s*\)|#[0-9a-fA-F]{3,8}\b|--[\w-]+|@[a-zA-Z-]+|(?:\d*\.)?\d+(?:%|[a-zA-Z]+)?|[.#]?[a-zA-Z_][\w-]*(?=\s*[:{,])|[{}:;(),]/gi;
  let output = '';
  let cursor = 0;
  for (const match of css.matchAll(tokenPattern)) {
    output += escapeHtml(css.slice(cursor, match.index));
    const token = match[0];
    if (token.startsWith('/*')) {
      output += highlightedComment(token, match.index);
      cursor = match.index + token.length;
      continue;
    }
    let type = 'punctuation';
    if (token.startsWith('"') || token.startsWith("'")) type = 'string';
    else if (normalizedPickerColor(token)) type = 'color';
    else if (token.startsWith('--')) type = 'variable';
    else if (token.startsWith('@')) type = 'at-rule';
    else if (/^(?:\d|\.\d)/.test(token)) type = 'number';
    else if (/^[.#]/.test(token)) type = 'selector';
    else if (/^[a-zA-Z_-]/.test(token)) {
      const following = css.slice(match.index + token.length).match(/^\s*(.)/)?.[1];
      type = following === ':' ? 'property' : 'selector';
    }
    const highlighted = `<span class="syntax-${type}">${escapeHtml(token)}</span>`;
    if (type === 'color') {
      output += colorDecorationHtml(token, match.index);
    } else {
      output += highlighted;
    }
    cursor = match.index + token.length;
  }
  output += escapeHtml(css.slice(cursor));
  highlightCode.innerHTML = output + (css.endsWith('\n') ? ' ' : '');
}

function ensureColorDecorationSpacing() {
  const decorations = [...highlightCode.querySelectorAll('.syntax-color-decoration')].reverse();
  let selectionStart = cssEditor.selectionStart;
  let selectionEnd = cssEditor.selectionEnd;
  let changed = false;
  for (const decoration of decorations) {
    const start = Number(decoration.dataset.start);
    let existingSpaces = 0;
    for (let index = start - 1; index >= 0 && cssEditor.value[index] === ' '; index -= 1) {
      existingSpaces += 1;
    }
    const spacesToAdd = Math.max(0, 3 - existingSpaces);
    if (!spacesToAdd) continue;
    cssEditor.setRangeText(' '.repeat(spacesToAdd), start, start, 'preserve');
    if (selectionStart >= start) selectionStart += spacesToAdd;
    if (selectionEnd >= start) selectionEnd += spacesToAdd;
    changed = true;
  }
  if (changed) cssEditor.setSelectionRange(selectionStart, selectionEnd);
  return changed;
}

function cssWithoutDecorationSpacing(rangeStart = 0, rangeEnd = cssEditor.value.length) {
  let output = cssEditor.value.slice(rangeStart, rangeEnd);
  const starts = [...highlightCode.querySelectorAll('.syntax-color-decoration')]
    .map(decoration => Number(decoration.dataset.start))
    .filter(start => start >= rangeStart && start <= rangeEnd)
    .sort((left, right) => right - left);
  for (const start of starts) {
    const localStart = start - rangeStart;
    let spaceCount = 0;
    for (let index = localStart - 1; index >= 0 && output[index] === ' '; index -= 1) {
      spaceCount += 1;
    }
    if (spaceCount > 1) {
      output = output.slice(0, localStart - spaceCount + 1) + output.slice(localStart);
    }
  }
  return output;
}

function sendThemePreview() {
  window.clearTimeout(previewTimer);
  previewTimer = null;
  return invoke('preview_theme_css', { css: cssWithoutDecorationSpacing() });
}

function scheduleThemePreview() {
  window.clearTimeout(previewTimer);
  previewTimer = window.setTimeout(() => {
    sendThemePreview().catch(error => setStatus(error?.message || String(error), true));
  }, 120);
}

function updateHighlight() {
  highlightCss(cssEditor.value);
  if (ensureColorDecorationSpacing()) highlightCss(cssEditor.value);
  const lineCount = cssEditor.value.split('\n').length;
  lineNumberContent.textContent = Array.from({ length: lineCount }, (_, index) => index + 1).join('\n');
  lineNumberContent.style.transform = `translateY(${-cssEditor.scrollTop}px)`;
  highlightLayer.scrollTop = cssEditor.scrollTop;
  highlightLayer.scrollLeft = cssEditor.scrollLeft;
  renderColorSwatches();
}

function normalizedPickerColor(value) {
  if (value.startsWith('#')) {
    const hex = value.slice(1);
    if (hex.length === 3 || hex.length === 4) {
      return `#${hex.slice(0, 3).split('').map(character => character + character).join('')}`;
    }
    if (hex.length === 6 || hex.length === 8) return `#${hex.slice(0, 6)}`;
  }
  if (/^rgba?\(/i.test(value)) {
    const channels = value.match(/\d+(?:\.\d+)?/g)?.slice(0, 3).map(channel => {
      return Math.max(0, Math.min(255, Math.round(Number(channel))));
    });
    if (channels?.length === 3) {
      return `#${channels.map(channel => channel.toString(16).padStart(2, '0')).join('')}`;
    }
  }
  return null;
}

function hexToRgb(hex) {
  const value = normalizedPickerColor(hex).slice(1);
  return {
    r: Number.parseInt(value.slice(0, 2), 16),
    g: Number.parseInt(value.slice(2, 4), 16),
    b: Number.parseInt(value.slice(4, 6), 16)
  };
}

function rgbToHex({ r, g, b }) {
  return `#${[r, g, b].map(channel => {
    return Math.max(0, Math.min(255, Math.round(channel))).toString(16).padStart(2, '0');
  }).join('')}`;
}

function rgbToHsv({ r, g, b }) {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const delta = max - min;
  let h = 0;
  if (delta) {
    if (max === r) h = 60 * (((g - b) / delta) % 6);
    else if (max === g) h = 60 * ((b - r) / delta + 2);
    else h = 60 * ((r - g) / delta + 4);
  }
  if (h < 0) h += 360;
  return { h, s: max ? delta / max : 0, v: max };
}

function hsvToRgb({ h, s, v }) {
  const chroma = v * s;
  const section = h / 60;
  const x = chroma * (1 - Math.abs((section % 2) - 1));
  const [r1, g1, b1] = section < 1 ? [chroma, x, 0]
    : section < 2 ? [x, chroma, 0]
      : section < 3 ? [0, chroma, x]
        : section < 4 ? [0, x, chroma]
          : section < 5 ? [x, 0, chroma]
            : [chroma, 0, x];
  const offset = v - chroma;
  return { r: (r1 + offset) * 255, g: (g1 + offset) * 255, b: (b1 + offset) * 255 };
}

function renderColorSwatches() {
  swatchLayer.replaceChildren();
  const editorBounds = swatchLayer.parentElement.getBoundingClientRect();
  for (const decoration of highlightCode.querySelectorAll('.syntax-color-decoration')) {
    const color = decoration.dataset.color;
    const bounds = decoration.getBoundingClientRect();
    const swatch = document.createElement('button');
    swatch.type = 'button';
    swatch.className = 'inline-color-swatch';
    swatch.style.background = color;
    swatch.style.left = `${bounds.left - editorBounds.left - 15}px`;
    swatch.style.top = `${bounds.top - editorBounds.top + (bounds.height - 12) / 2}px`;
    const sourceColor = cssEditor.value.slice(Number(decoration.dataset.start), Number(decoration.dataset.end));
    swatch.title = `Edit ${sourceColor}`;
    swatch.setAttribute('aria-label', `Edit color ${sourceColor}`);
    swatch.addEventListener('click', event => {
      event.stopPropagation();
      openColorPopover(swatch, {
        start: Number(decoration.dataset.start),
        end: Number(decoration.dataset.end)
      }, color);
    });
    swatchLayer.appendChild(swatch);
  }
}

function closeColorPopover() {
  colorPopover?.element.remove();
  colorPopover = null;
}

function paintColorPopover() {
  if (!colorPopover) return;
  const color = rgbToHex(hsvToRgb(colorPopover.hsv));
  colorPopover.field.style.backgroundColor = `hsl(${colorPopover.hsv.h} 100% 50%)`;
  colorPopover.svThumb.style.left = `${colorPopover.hsv.s * 100}%`;
  colorPopover.svThumb.style.top = `${(1 - colorPopover.hsv.v) * 100}%`;
  colorPopover.hueThumb.style.top = `${colorPopover.hsv.h / 360 * 100}%`;
  colorPopover.preview.style.background = color;
  colorPopover.input.value = color;
}

function replacePopoverColor() {
  if (!colorPopover) return;
  const color = rgbToHex(hsvToRgb(colorPopover.hsv));
  const { range } = colorPopover;
  cssEditor.setRangeText(color, range.start, range.end, 'end');
  range.end = range.start + color.length;
  updateHighlight();
  scheduleThemePreview();
  paintColorPopover();
}

function trackPointer(element, initialEvent, callback) {
  initialEvent.preventDefault();
  element.setPointerCapture(initialEvent.pointerId);
  const update = event => callback(event, element.getBoundingClientRect());
  const finish = event => {
    update(event);
    element.releasePointerCapture(event.pointerId);
    element.removeEventListener('pointermove', update);
    element.removeEventListener('pointerup', finish);
  };
  element.addEventListener('pointermove', update);
  element.addEventListener('pointerup', finish);
  update(initialEvent);
}

function openColorPopover(anchor, range, color) {
  closeColorPopover();
  const hsv = rgbToHsv(hexToRgb(color));
  const element = document.createElement('div');
  element.className = 'inline-color-popover';
  element.innerHTML = '<div class="inline-color-header"><span class="inline-color-preview"></span><input class="inline-color-value" aria-label="Color value" spellcheck="false"></div><div class="inline-color-body"><div class="inline-color-sv"><span class="inline-color-thumb"></span></div><div class="inline-color-hue"><span class="inline-hue-thumb"></span></div></div>';
  document.body.appendChild(element);
  const bounds = anchor.getBoundingClientRect();
  const popoverWidth = 264;
  const popoverHeight = 192;
  element.style.left = `${Math.max(8, Math.min(window.innerWidth - popoverWidth - 8, bounds.left - 8))}px`;
  element.style.top = `${bounds.bottom + popoverHeight + 8 < window.innerHeight ? bounds.bottom + 7 : Math.max(8, bounds.top - popoverHeight - 7)}px`;
  colorPopover = {
    element,
    range,
    hsv,
    preview: element.querySelector('.inline-color-preview'),
    input: element.querySelector('.inline-color-value'),
    field: element.querySelector('.inline-color-sv'),
    svThumb: element.querySelector('.inline-color-thumb'),
    hue: element.querySelector('.inline-color-hue'),
    hueThumb: element.querySelector('.inline-hue-thumb')
  };
  colorPopover.field.addEventListener('pointerdown', event => trackPointer(colorPopover.field, event, (pointer, rect) => {
    colorPopover.hsv.s = Math.max(0, Math.min(1, (pointer.clientX - rect.left) / rect.width));
    colorPopover.hsv.v = 1 - Math.max(0, Math.min(1, (pointer.clientY - rect.top) / rect.height));
    replacePopoverColor();
  }));
  colorPopover.hue.addEventListener('pointerdown', event => trackPointer(colorPopover.hue, event, (pointer, rect) => {
    colorPopover.hsv.h = Math.max(0, Math.min(359.999, (pointer.clientY - rect.top) / rect.height * 360));
    replacePopoverColor();
  }));
  colorPopover.input.addEventListener('change', () => {
    const normalized = normalizedPickerColor(colorPopover.input.value.trim());
    if (!normalized) {
      paintColorPopover();
      return;
    }
    colorPopover.hsv = rgbToHsv(hexToRgb(normalized));
    replacePopoverColor();
  });
  colorPopover.input.addEventListener('keydown', event => {
    if (event.key === 'Enter') {
      event.preventDefault();
      colorPopover.input.dispatchEvent(new Event('change'));
      colorPopover.input.select();
    }
  });
  paintColorPopover();
}

function setStatus(message, error = false) {
  status.textContent = message;
  status.classList.toggle('error', error);
}

async function closeEditor() {
  window.clearTimeout(previewTimer);
  try {
    await invoke('clear_theme_preview');
  } finally {
    invoke('close_theme_editor').catch(() => window.close());
  }
}

function setSelectingElement(selecting) {
  selectingElement = selecting;
  selectElementButton.disabled = selecting;
  selectElementButton.textContent = selecting ? 'Selecting…' : 'Select Launcher Element';
  if (selecting) setStatus('Click an element in the launcher, or press Escape to cancel.');
  else if (!status.classList.contains('error')) setStatus('Ctrl+S to save');
}

function insertSelectorBlock(selector) {
  const start = cssEditor.selectionStart;
  const end = cssEditor.selectionEnd;
  const needsLeadingSpace = start > 0 && !cssEditor.value.slice(0, start).endsWith('\n\n');
  const insertion = `${needsLeadingSpace ? '\n\n' : ''}${selector} {\n  \n}`;
  cssEditor.setRangeText(insertion, start, end, 'end');
  const cursor = start + insertion.indexOf('\n  ') + 3;
  cssEditor.setSelectionRange(cursor, cursor);
  updateHighlight();
  scheduleThemePreview();
  cssEditor.focus();
}

async function saveTheme() {
  if (saving) return;
  saving = true;
  saveButton.disabled = true;
  setStatus('Saving…');
  try {
    window.clearTimeout(previewTimer);
    const snapshot = await invoke('save_theme', {
      originalId,
      name: nameInput.value,
      css: cssWithoutDecorationSpacing()
    });
    originalId = snapshot.selectedTheme;
    document.querySelector('#editor-title').textContent = `Edit ${nameInput.value.trim()}`;
    setStatus('Saved');
    await sendThemePreview();
  } catch (error) {
    setStatus(error?.message || String(error), true);
  } finally {
    saving = false;
    saveButton.disabled = false;
  }
}

saveButton.addEventListener('click', saveTheme);
selectElementButton.addEventListener('click', async () => {
  if (selectingElement) return;
  setSelectingElement(true);
  try {
    await invoke('begin_theme_element_picker');
  } catch (error) {
    setSelectingElement(false);
    setStatus(error?.message || String(error), true);
  }
});
cssEditor.addEventListener('input', () => {
  updateHighlight();
  scheduleThemePreview();
});
cssEditor.addEventListener('copy', event => {
  const start = cssEditor.selectionStart;
  const end = cssEditor.selectionEnd;
  if (end <= start) return;
  event.preventDefault();
  event.clipboardData.setData('text/plain', cssWithoutDecorationSpacing(start, end));
});
cssEditor.addEventListener('cut', event => {
  const start = cssEditor.selectionStart;
  const end = cssEditor.selectionEnd;
  if (end <= start) return;
  event.preventDefault();
  event.clipboardData.setData('text/plain', cssWithoutDecorationSpacing(start, end));
  cssEditor.setRangeText('', start, end, 'end');
  updateHighlight();
  scheduleThemePreview();
});
cssEditor.addEventListener('scroll', () => {
  highlightLayer.scrollTop = cssEditor.scrollTop;
  highlightLayer.scrollLeft = cssEditor.scrollLeft;
  lineNumberContent.style.transform = `translateY(${-cssEditor.scrollTop}px)`;
  renderColorSwatches();
});
document.querySelector('#cancel-theme').addEventListener('click', closeEditor);
document.addEventListener('pointerdown', event => {
  if (colorPopover && !event.target.closest('.inline-color-popover, .inline-color-swatch')) {
    closeColorPopover();
  }
});
document.addEventListener('keydown', event => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
    event.preventDefault();
    saveTheme();
  }
  if (event.key === 'Tab' && event.target === cssEditor) {
    event.preventDefault();
    const start = cssEditor.selectionStart;
    cssEditor.setRangeText('  ', start, cssEditor.selectionEnd, 'end');
    updateHighlight();
    scheduleThemePreview();
  }
  if (event.key === 'Escape') {
    if (colorPopover) {
      event.preventDefault();
      closeColorPopover();
      cssEditor.focus();
    } else {
      closeEditor();
    }
  }
});

listen('theme-element-selected', event => {
  setSelectingElement(false);
  insertSelectorBlock(event.payload);
  setStatus(`Added ${event.payload}`);
});

listen('theme-element-picker-cancelled', () => {
  setSelectingElement(false);
  cssEditor.focus();
});

invoke('get_theme_for_edit', { themeId }).then(themeDocument => {
  originalId = themeDocument.id;
  nameInput.value = themeDocument.name;
  cssEditor.value = themeDocument.css;
  updateHighlight();
  document.querySelector('#editor-title').textContent = originalId ? `Edit ${themeDocument.name}` : 'New Theme';
  cssEditor.focus();
}).catch(error => {
  setStatus(error?.message || String(error), true);
  saveButton.disabled = true;
});
