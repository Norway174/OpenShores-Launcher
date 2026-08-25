'use strict';

const { invoke } = window.__TAURI__.core;
const title = document.querySelector('#changelog-title');
const content = document.querySelector('#changelog-content');
const openRelease = document.querySelector('#open-release');
let releaseUrl = '';

function escapeHtml(value) {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
}

function inlineMarkdown(value) {
  const links = [];
  const withLinkTokens = escapeHtml(value).replace(
    /\[([^\]]+)\]\((https:\/\/[^\s)]+)\)|(https:\/\/[^\s<]+[^\s<.,;:!?])/g,
    (_, label, markdownUrl, plainUrl) => {
      const url = markdownUrl || plainUrl;
      const text = label || plainUrl;
      const token = `\u0000LINK${links.length}\u0000`;
      links.push(`<a href="${url}" data-external="${url}">${text}</a>`);
      return token;
    },
  );

  return withLinkTokens
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/\u0000LINK(\d+)\u0000/g, (_, index) => links[Number(index)]);
}

function renderMarkdown(markdown) {
  const output = [];
  let listOpen = false;
  const closeList = () => {
    if (listOpen) output.push('</ul>');
    listOpen = false;
  };
  for (const rawLine of markdown.replaceAll('\r\n', '\n').split('\n')) {
    const line = rawLine.trimEnd();
    const heading = line.match(/^(#{1,3})\s+(.+)$/);
    const listItem = line.match(/^[-*]\s+(.+)$/);
    if (heading) {
      closeList();
      const level = heading[1].length;
      output.push(`<h${level}>${inlineMarkdown(heading[2])}</h${level}>`);
    } else if (listItem) {
      if (!listOpen) output.push('<ul>');
      listOpen = true;
      output.push(`<li>${inlineMarkdown(listItem[1])}</li>`);
    } else if (/^---+$/.test(line)) {
      closeList();
      output.push('<hr>');
    } else if (line.trim()) {
      closeList();
      output.push(`<p>${inlineMarkdown(line)}</p>`);
    } else {
      closeList();
    }
  }
  closeList();
  return output.join('');
}

content.addEventListener('click', event => {
  const link = event.target.closest('[data-external]');
  if (!link) return;
  event.preventDefault();
  invoke('open_link', { url: link.dataset.external });
});

openRelease.addEventListener('click', () => {
  if (releaseUrl) invoke('open_link', { url: releaseUrl });
});

document.querySelector('#close-changelog').addEventListener('click', () => {
  invoke('close_changelog').catch(() => window.close());
});

invoke('get_changelog').then(data => {
  title.textContent = data.title;
  releaseUrl = data.releaseUrl;
  content.innerHTML = renderMarkdown(data.changelog);
}).catch(error => {
  title.textContent = 'Changelog unavailable';
  content.textContent = error?.message || String(error);
  openRelease.disabled = true;
});
