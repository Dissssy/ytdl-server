'use strict';
// Starts downloads on the server, follows them by polling while any are active, and saves
// finished files through Firefox's download manager (or leaves them on the NAS).

const DEFAULTS = { server: 'https://ytdlp.deadlyneurotox.in', key: '', videoMode: 'compat', videoCap: 0, notify: true };
let settings = { ...DEFAULTS };
let me = null;                 // { user, nas } from /api/me
let pollTimer = null;
const ACTIVE = ['queued', 'fetching', 'processing'];
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
  poll();
}
browser.storage.onChanged.addListener(load);

async function api(path, opts = {}) {
  const res = await fetch(settings.server + path, {
    ...opts,
    headers: { Authorization: `Bearer ${settings.key}`, ...(opts.body ? { 'Content-Type': opts.body.startsWith('#') ? 'text/plain' : 'application/json' } : {}) },
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
    poll();
    if (closeTab && tabId != null) browser.tabs.remove(tabId).catch(() => {});
  } catch (err) {
    toast(tabId, { id: 'error-' + Date.now(), state: 'failed', title: url, error: err });
    notify('Download failed', err.message);
  }
}

// From the page (Gesturify functions) and the popup.
browser.runtime.onMessage.addListener((msg, sender) => {
  if (msg.type === 'download') return start(msg.url, msg.audio, sender.tab?.id, !!msg.close);
  if (msg.type === 'state') {
    poll();
    return Promise.resolve({ me, server: settings.server, jobs: [...jobs.values()].sort((a, b) => b.created - a.created) });
  }
  if (msg.type === 'cancel') return api(`/api/jobs/${msg.id}`, { method: 'DELETE' }).catch(() => {});
  if (msg.type === 'save') return save(msg.id);
  if (msg.type === 'open-site') return browser.tabs.create({ url: `${settings.server}/${msg.path || ''}` });
  // The site's Settings dialog shows and edits these when the extension is installed.
  const fromSite = sender.url?.startsWith(settings.server + '/');
  if (msg.type === 'get-settings' && fromSite) {
    return Promise.resolve({ videoMode: settings.videoMode, videoCap: settings.videoCap, notify: settings.notify, version: browser.runtime.getManifest().version });
  }
  // Only ever sent by a button press on the options page or the site's Settings.
  if (msg.type === 'upload-cookies' && (fromSite || sender.url?.startsWith(browser.runtime.getURL('')))) {
    return uploadCookies(msg.domains).then((r) => ({ ok: true, ...r }), (e) => ({ ok: false, message: e.message || String(e), detail: e.detail || '' }));
  }
  if (msg.type === 'set-settings' && fromSite) {
    const v = msg.settings || {};
    return browser.storage.local.set({
      videoMode: v.videoMode === 'best' ? 'best' : 'compat',
      videoCap: [0, 10, 25].includes(v.videoCap) ? v.videoCap : 0,
      notify: !!v.notify,
    });
  }
  // The site's own page shares its stored key, so visiting it once signs the extension in.
  if (msg.type === 'site-key' && msg.key && msg.key !== settings.key && sender.url?.startsWith(settings.server)) {
    return browser.storage.local.set({ key: msg.key });
  }
});

// ---------- cookies: exported from Firefox only when the user presses Upload ----------
async function cookiesFor(domain) {
  try {
    return await browser.cookies.getAll({ domain });
  } catch (e) {
    // With first-party isolation on, Firefox insists on the firstPartyDomain field.
    if (String(e).includes('firstPartyDomain')) return browser.cookies.getAll({ domain, firstPartyDomain: null });
    throw e;
  }
}
async function uploadCookies(domains) {
  const list = (domains || []).map((d) => String(d).trim().replace(/^\.+/, '')).filter(Boolean);
  if (!list.length) throw { message: 'Name at least one site, like youtube.com.' };
  const seen = new Set();
  const lines = ['# Netscape HTTP Cookie File'];
  for (const d of list) {
    for (const c of await cookiesFor(d)) {
      const id = `${c.domain}\t${c.path}\t${c.name}`;
      if (seen.has(id)) continue;
      seen.add(id);
      const domain = c.hostOnly ? c.domain : (c.domain.startsWith('.') ? c.domain : '.' + c.domain);
      lines.push([
        (c.httpOnly ? '#HttpOnly_' : '') + domain,
        c.hostOnly ? 'FALSE' : 'TRUE',
        c.path,
        c.secure ? 'TRUE' : 'FALSE',
        c.expirationDate ? Math.floor(c.expirationDate) : 0,
        c.name,
        c.value,
      ].join('\t'));
    }
  }
  if (lines.length === 1) throw { message: `No cookies found for ${list.join(', ')}. Sign in to the site in this browser first.` };
  return api('/api/cookies', { method: 'POST', body: lines.join('\n') + '\n' });
}

// ---------- following jobs ----------
// Polling (rather than an EventSource) is dependable from an extension background page,
// and it only runs while something is in progress.
async function poll() {
  clearTimeout(pollTimer);
  pollTimer = null;
  if (!me) return;
  try {
    const list = await api('/api/jobs');
    const ids = new Set(list.map((j) => j.id));
    for (const [id, j] of jobs) if (!ids.has(id)) seen({ ...j, state: 'expired' });
    for (const j of list) seen(j);
  } catch (e) {
    console.warn('poll failed', e);
  }
  const busy = tracked.size > 0 || [...jobs.values()].some((j) => ACTIVE.includes(j.state));
  if (busy) pollTimer = setTimeout(poll, 1500);
}

function seen(job) {
  const prev = jobs.get(job.id);
  if (job.state === 'expired') jobs.delete(job.id); else jobs.set(job.id, job);
  badge();
  if (JSON.stringify(prev) === JSON.stringify(job)) return;
  browser.runtime.sendMessage({ type: 'job', job }).catch(() => {}); // popup, if open
  const t = tracked.get(job.id);
  if (!t) return;
  toast(t.tabId, job);
  if (prev?.state === job.state) return;
  if (job.state === 'ready') { save(job.id); tracked.delete(job.id); }
  if (job.state === 'done') { notify('Saved to NAS', job.title || job.url); tracked.delete(job.id); }
  if (job.state === 'failed') {
    notify('Download failed', `${job.title || job.url}\n${job.error?.message || ''}`);
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
  const active = [...jobs.values()].filter((j) => ACTIVE.includes(j.state)).length;
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
