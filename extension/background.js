'use strict';
// Starts downloads on the server, follows them over server-sent events, and saves
// finished files through Firefox's download manager (or leaves them on the NAS).

const DEFAULTS = { server: 'https://ytdlp.deadlyneurotox.in', key: '', videoMode: 'compat', videoCap: 0, notify: true };
let settings = { ...DEFAULTS };
let me = null;                 // { user, nas } from /api/me
let events = null;
const tracked = new Map();     // job id -> { tabId, closeTab }
const jobs = new Map();        // job id -> latest view, for the popup

async function load() {
  settings = { ...DEFAULTS, ...(await browser.storage.local.get(Object.keys(DEFAULTS))) };
  settings.server = settings.server.replace(/\/+$/, '');
  me = null;
  if (settings.key) {
    try { me = await api('/api/me'); } catch (e) { console.warn('sign-in failed', e); }
  }
  menus();
  connect();
}
browser.storage.onChanged.addListener(load);

async function api(path, opts = {}) {
  const res = await fetch(settings.server + path, {
    ...opts,
    headers: { Authorization: `Bearer ${settings.key}`, ...(opts.body ? { 'Content-Type': 'application/json' } : {}) },
  });
  const text = await res.text();
  let data; try { data = JSON.parse(text); } catch { data = { message: text || res.statusText }; }
  if (!res.ok) throw { message: data.message || 'Request failed.', detail: data.detail || '' };
  return data;
}

// ---------- context menu ----------
function menus() {
  browser.menus.removeAll();
  const where = me?.nas ? ' to NAS' : '';
  const contexts = ['page', 'link', 'video', 'audio', 'image'];
  if (!me) {
    browser.menus.create({ id: 'setup', title: 'Media Downloader: sign in…', contexts: ['page', 'link', 'video'] });
    return;
  }
  browser.menus.create({ id: 'video', title: `Download video${where}`, contexts });
  browser.menus.create({ id: 'audio', title: `Download audio${where}`, contexts });
  browser.menus.create({ id: 'pick', title: 'Choose quality…', contexts });
}

function targetUrl(info, tab) {
  if (info.linkUrl) return info.linkUrl;
  // A <video> src is often a blob: or a CDN segment; the page is what yt-dlp understands.
  if (info.srcUrl && /^https?:/.test(info.srcUrl) && info.mediaType !== 'video') return info.srcUrl;
  return info.pageUrl || tab?.url;
}

browser.menus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === 'setup') return browser.runtime.openOptionsPage();
  const url = targetUrl(info, tab);
  if (info.menuItemId === 'pick') return browser.tabs.create({ url: `${settings.server}/?url=${encodeURIComponent(url)}` });
  start(url, info.menuItemId === 'audio', tab?.id, false);
});

// ---------- starting jobs ----------
async function start(url, audio, tabId, closeTab) {
  if (!me) {
    notify('Sign in first', 'Open your invite link, or set your key in the extension settings.');
    browser.runtime.openOptionsPage();
    return;
  }
  const body = {
    url,
    mode: audio ? 'audio' : settings.videoMode,
    max_size: !audio && settings.videoCap ? settings.videoCap * 1024 * 1024 : null,
    target: 'auto', // the NAS for NAS users, the browser for everyone else
  };
  try {
    const job = await api('/api/jobs', { method: 'POST', body: JSON.stringify(body) });
    tracked.set(job.id, { tabId, closeTab });
    seen(job);
    if (closeTab && tabId != null) browser.tabs.remove(tabId).catch(() => {});
  } catch (err) {
    toast(tabId, { id: 'error-' + Date.now(), state: 'failed', title: url, error: err });
    notify('Download failed', err.message);
  }
}

// From the page (Gesturify functions) and the popup.
browser.runtime.onMessage.addListener((msg, sender) => {
  if (msg.type === 'download') return start(msg.url, msg.audio, sender.tab?.id, !!msg.close);
  if (msg.type === 'state') return Promise.resolve({ me, server: settings.server, jobs: [...jobs.values()].sort((a, b) => b.created - a.created) });
  if (msg.type === 'cancel') return api(`/api/jobs/${msg.id}`, { method: 'DELETE' }).catch(() => {});
  if (msg.type === 'save') return save(msg.id);
  // The site's own page shares its stored key, so visiting it once signs the extension in.
  if (msg.type === 'site-key' && msg.key && msg.key !== settings.key && sender.url?.startsWith(settings.server)) {
    return browser.storage.local.set({ key: msg.key });
  }
});

// ---------- following jobs ----------
function connect() {
  events?.close();
  events = null;
  if (!me) return;
  events = new EventSource(`${settings.server}/api/events?key=${encodeURIComponent(settings.key)}`);
  events.addEventListener('snapshot', (e) => {
    jobs.clear();
    for (const j of JSON.parse(e.data)) jobs.set(j.id, j);
    badge();
  });
  events.addEventListener('job', (e) => seen(JSON.parse(e.data)));
}

function seen(job) {
  const prev = jobs.get(job.id);
  if (job.state === 'expired') jobs.delete(job.id); else jobs.set(job.id, job);
  badge();
  browser.runtime.sendMessage({ type: 'job', job }).catch(() => {}); // popup, if open
  const t = tracked.get(job.id);
  if (!t) return;
  toast(t.tabId, job);
  if (prev?.state === job.state) return;
  if (job.state === 'ready') { save(job.id); tracked.delete(job.id); }
  if (job.state === 'done') { notify('Saved to NAS', job.title || job.url); tracked.delete(job.id); }
  if (job.state === 'failed') {
    notify('Download failed', `${job.title || job.url}\n${job.error?.message || ''}`);
    // Point at the upload button; never upload anything on the user's behalf.
    if (job.error?.hint === 'cookies') browser.runtime.openOptionsPage();
    tracked.delete(job.id);
  }
  if (job.state === 'cancelled') tracked.delete(job.id);
}

async function save(id) {
  const job = jobs.get(id);
  try {
    await browser.downloads.download({
      url: `${settings.server}/api/jobs/${id}/file?key=${encodeURIComponent(settings.key)}`,
      filename: job?.filename?.replace(/[\\/:*?"<>|]/g, '_') || undefined,
    });
    notify('Download ready', job?.title || 'Saved to your Downloads folder.');
  } catch (err) {
    notify('Saving failed', String(err));
  }
}

function badge() {
  const active = [...jobs.values()].filter((j) => ['queued', 'fetching', 'processing'].includes(j.state)).length;
  browser.browserAction.setBadgeText({ text: active ? String(active) : '' });
  browser.browserAction.setBadgeBackgroundColor({ color: '#6d5dfc' });
}

function toast(tabId, job) {
  if (tabId == null) return;
  browser.tabs.sendMessage(tabId, { type: 'toast', job }).catch(() => {});
}

function notify(title, message) {
  if (!settings.notify) return;
  browser.notifications.create({ type: 'basic', iconUrl: 'icon.svg', title, message: message || '' });
}

load();
