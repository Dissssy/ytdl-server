'use strict';
const $ = (id) => document.getElementById(id);
const fmt = (b) => b == null ? '' : b < 1048576 ? `${Math.round(b / 1024)} KB` : `${(b / 1048576).toFixed(1)} MB`;
const LABEL = { queued: 'Queued', fetching: 'Downloading', processing: 'Processing', ready: 'Ready', saved: 'Saved', done: 'On NAS', failed: 'Failed', cancelled: 'Cancelled' };
let state = null;

async function tabUrl() {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  return tab;
}

async function init() {
  state = await browser.runtime.sendMessage({ type: 'state' });
  $('signed-out').hidden = !!state.me;
  $('signed-in').hidden = !state.me;
  if (!state.me) return;
  $('who').textContent = state.me.user + (state.me.nas ? ' · saves to NAS' : '');
  render();
}

function sub(j) {
  const p = j.progress || {};
  if (j.state === 'fetching' && p.total) return `${Math.floor(p.downloaded / p.total * 100)}% · ${fmt(p.downloaded)} of ${fmt(p.total)}`;
  if (j.state === 'processing' && p.processing != null) return `Converting · ${Math.floor(p.processing * 100)}%`;
  if (j.state === 'failed') return j.error?.message || 'Failed';
  if (['ready', 'saved', 'done'].includes(j.state)) return [LABEL[j.state], j.choice, fmt(j.size)].filter(Boolean).join(' · ');
  return LABEL[j.state] || j.state;
}

function render() {
  const ul = $('jobs');
  ul.innerHTML = '';
  $('empty').hidden = state.jobs.length > 0;
  for (const j of state.jobs.slice(0, 20)) {
    const li = document.createElement('li');
    li.innerHTML = '<div class="top"><span class="title"></span><button class="x" title="Cancel or remove">✕</button></div><div class="sub"></div>';
    li.querySelector('.title').textContent = j.title || j.url;
    li.querySelector('.title').title = j.url;
    const s = li.querySelector('.sub');
    s.textContent = sub(j);
    if (j.state === 'failed') s.className = 'sub err';
    li.querySelector('.x').onclick = () => browser.runtime.sendMessage({ type: 'cancel', id: j.id });
    if (['queued', 'fetching', 'processing'].includes(j.state)) {
      const bar = document.createElement('div');
      bar.className = 'bar';
      const fill = document.createElement('div');
      const p = j.progress || {};
      const frac = j.state === 'fetching' && p.total ? p.downloaded / p.total : j.state === 'processing' ? p.processing : null;
      fill.style.width = frac == null ? '15%' : `${frac * 100}%`;
      bar.append(fill);
      li.append(bar);
    }
    if (['ready', 'saved'].includes(j.state)) {
      const b = document.createElement('button');
      b.className = 'act';
      b.textContent = j.state === 'saved' ? 'Save again' : 'Save';
      b.onclick = () => browser.runtime.sendMessage({ type: 'save', id: j.id });
      li.append(b);
    }
    ul.append(li);
  }
}

browser.runtime.onMessage.addListener((msg) => {
  if (msg.type !== 'job' || !state) return;
  const i = state.jobs.findIndex((j) => j.id === msg.job.id);
  if (msg.job.state === 'expired') { if (i >= 0) state.jobs.splice(i, 1); }
  else if (i >= 0) state.jobs[i] = msg.job;
  else state.jobs.unshift(msg.job);
  render();
});

$('open-options').onclick = () => { browser.runtime.openOptionsPage(); window.close(); };
$('dl-video').onclick = async () => { const t = await tabUrl(); browser.runtime.sendMessage({ type: 'download', audio: false, url: t.url }); };
$('dl-audio').onclick = async () => { const t = await tabUrl(); browser.runtime.sendMessage({ type: 'download', audio: true, url: t.url }); };
$('open-site').onclick = async (e) => {
  e.preventDefault();
  const t = await tabUrl();
  browser.tabs.create({ url: `${state.server}/?url=${encodeURIComponent(t.url)}` });
  window.close();
};
init();
