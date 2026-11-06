'use strict';

const $ = (id) => document.getElementById(id);
const store = {
  get(k, d = null) { try { return localStorage.getItem(k) ?? d; } catch { return d; } },
  set(k, v) { try { v == null ? localStorage.removeItem(k) : localStorage.setItem(k, v); } catch {} },
};
const MB = 1024 * 1024;
const PRESETS = {
  share: { mode: 'compat', cap: 25 * MB },
  discord: { mode: 'compat', cap: 10 * MB },
  best: { mode: 'best', cap: null },
  audio: { mode: 'audio', cap: null },
};

let key = store.get('api_key');
let me = null;
let probe = null;        // last /api/probe response
let probeSeq = 0;
let preset = store.get('preset', 'share');
let cap = PRESETS[preset]?.cap ?? 25 * MB;
const jobs = new Map();
const mine = new Set(JSON.parse(sessionStorage.getItem('mine') || '[]'));
let events = null;

// ---------- formatting ----------
function size(b, approx) {
  if (b == null) return 'size unknown';
  const t = b < MB ? `${Math.max(1, Math.round(b / 1024))} KB` : b < 1024 * MB ? `${(b / MB).toFixed(b < 10 * MB ? 1 : 0)} MB` : `${(b / 1024 / MB).toFixed(2)} GB`;
  return approx ? `~${t}` : t;
}
function dur(s) {
  if (s == null || !isFinite(s)) return '';
  s = Math.max(1, Math.round(s));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60), r = s % 60;
  if (m < 60) return r ? `${m}m ${r}s` : `${m}m`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}
function clock(s) {
  if (!s) return '';
  s = Math.round(s);
  const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, r = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${r}` : `${m}:${r}`;
}

// Fits a title to its box: an ellipsis at rest, and a slow back-and-forth scroll when it doesn't fit.
const marqueeObserver = typeof ResizeObserver === 'function' ? new ResizeObserver((entries) => entries.forEach((e) => measureMarquee(e.target))) : null;
function marquee(el, text) {
  let span = el.firstElementChild;
  if (!span || !el.classList.contains('marquee')) {
    span = document.createElement('span');
    el.textContent = '';
    el.append(span);
    el.classList.add('marquee');
    marqueeObserver?.observe(el);
  }
  if (span.textContent !== text) {
    span.textContent = text;
    measureMarquee(el);
  }
}
function measureMarquee(el) {
  const span = el.firstElementChild;
  if (!span) return;
  el.classList.remove('run');
  const shift = Math.ceil(span.getBoundingClientRect().width - el.clientWidth);
  if (shift > 2 && el.clientWidth > 0) {
    el.style.setProperty('--shift', `${shift}px`);
    el.style.setProperty('--dur', `${Math.max(4, shift / 25 + 2).toFixed(1)}s`);
    el.classList.add('run');
  }
}

// ---------- API ----------
async function api(path, opts = {}) {
  const res = await fetch(path, {
    ...opts,
    headers: { 'Authorization': `Bearer ${key}`, ...(opts.body ? { 'Content-Type': 'application/json' } : {}), ...opts.headers },
  });
  if (res.status === 401) { signOut('Your key stopped working. Use your invite link again.'); throw { message: 'Signed out.' }; }
  if (res.status === 204) return null;
  const text = await res.text();
  let data; try { data = JSON.parse(text); } catch { data = { message: text || res.statusText, detail: '' }; }
  if (!res.ok) throw { message: data.message || 'Request failed.', detail: data.detail || '' };
  return data;
}

// ---------- sign in ----------
function parseKey(v) {
  v = v.trim();
  try { const u = new URL(v); return u.searchParams.get('key') || v; } catch { return v; }
}
async function signIn(k) {
  key = k;
  me = await api('/api/me');
  store.set('api_key', key);
  $('signin').hidden = true;
  $('app').hidden = false;
  $('who').hidden = false;
  $('who').innerHTML = '';
  $('who').append(me.user);
  if (me.nas) { const s = document.createElement('span'); s.className = 'nas'; s.textContent = 'NAS'; $('who').append(s); }
  $('nas-row').hidden = !me.nas;
  connect();
  window.postMessage({ source: 'md-page', type: 'signed-in' }, location.origin);
  $('url').focus();
}
function signOut(msg) {
  store.set('api_key', null);
  key = null; me = null;
  events?.close(); events = null;
  $('app').hidden = true; $('who').hidden = true; $('signin').hidden = false;
  $('signin-error').hidden = !msg; $('signin-error').textContent = msg || '';
}
$('signin-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  $('signin-error').hidden = true;
  try { await signIn(parseKey($('signin-key').value)); }
  catch (err) { if (key) signOut(err.message === 'Signed out.' ? "That key isn't valid." : err.message); }
});

// ---------- probing ----------
const looksLikeUrl = (v) => /^https?:\/\/\S+\.\S+/i.test(v.trim());
let probeTimer = 0;
$('url').addEventListener('input', () => {
  clearTimeout(probeTimer);
  const v = $('url').value;
  if (!v.trim()) return resetProbe();
  if (looksLikeUrl(v)) probeTimer = setTimeout(runProbe, 350);
});
$('url').addEventListener('keydown', (e) => { if (e.key === 'Enter' && !probe) { e.preventDefault(); runProbe(); } });
if (navigator.clipboard?.readText) {
  $('paste').addEventListener('click', async () => {
    try { $('url').value = (await navigator.clipboard.readText()).trim(); runProbe(); } catch {}
  });
} else $('paste').hidden = true;

function resetProbe() {
  probeSeq++;
  probe = null;
  for (const id of ['preview', 'skeleton', 'options', 'probe-error']) $(id).hidden = true;
}
async function runProbe() {
  const url = $('url').value.trim();
  if (!url) return;
  const seq = ++probeSeq;
  probe = null;
  $('options').hidden = true; $('preview').hidden = true; $('probe-error').hidden = true;
  $('skeleton').hidden = false;
  try {
    const data = await api('/api/probe', { method: 'POST', body: JSON.stringify({ url }) });
    if (seq !== probeSeq) return;
    probe = { ...data, url };
    showPreview();
  } catch (err) {
    if (seq !== probeSeq) return;
    $('skeleton').hidden = true;
    showError($('probe-error'), err);
  }
}
function showError(el, err) {
  el.innerHTML = '';
  el.append(err.message || 'Something went wrong.');
  if (err.detail) {
    const d = document.createElement('details');
    d.innerHTML = '<summary>Details</summary>';
    const c = document.createElement('code'); c.textContent = err.detail; d.append(c);
    el.append(d);
  }
  el.hidden = false;
}
function showPreview() {
  const i = probe.info;
  $('skeleton').hidden = true;
  $('thumb').src = i.thumbnail || '';
  $('thumb').hidden = !i.thumbnail;
  $('title').textContent = i.title;
  $('sub').textContent = [i.uploader, i.site, clock(i.duration)].filter(Boolean).join(' · ');
  $('preview').hidden = false;
  $('options').hidden = false;
  if (!PRESETS[preset]) preset = 'share';
  applyPreset(preset);
}

// ---------- options ----------
const mode = () => preset === 'audio' ? 'audio' : (preset === 'best' || $('original').checked) ? 'best' : 'compat';
const ladder = () => probe?.ladders[mode()] ?? [];

// Log-scale slider from 1 MB to a bit above the largest option; the far right means no limit.
function sliderMax() {
  const biggest = Math.max(0, ...ladder().map((c) => c.size || 0));
  return Math.min(me?.max_file_size || Infinity, Math.max(50 * MB, biggest * 1.1));
}
const posToBytes = (p) => p >= 1000 ? null : Math.round(Math.exp(Math.log(MB) + (Math.log(sliderMax()) - Math.log(MB)) * p / 1000));
const bytesToPos = (b) => b == null ? 1000 : Math.max(0, Math.min(999, Math.round((Math.log(b) - Math.log(MB)) / (Math.log(sliderMax()) - Math.log(MB)) * 1000)));

function applyPreset(name) {
  preset = name;
  store.set('preset', name);
  if (PRESETS[name]) cap = PRESETS[name].cap;
  if (name === 'best') $('original').checked = true;
  if (name === 'share' || name === 'discord') $('original').checked = false;
  for (const b of document.querySelectorAll('[data-preset]')) b.setAttribute('aria-checked', String(b.dataset.preset === name));
  fillFormats();
  $('size').value = bytesToPos(cap);
  update();
}
document.querySelectorAll('[data-preset]').forEach((b) => b.addEventListener('click', () => applyPreset(b.dataset.preset)));
$('size').addEventListener('input', () => {
  cap = posToBytes(+$('size').value);
  if (preset !== 'custom') {
    preset = 'custom';
    for (const b of document.querySelectorAll('[data-preset]')) b.setAttribute('aria-checked', 'false');
  }
  $('format').value = '';
  update();
});
$('original').addEventListener('change', () => {
  if (preset === 'audio') preset = 'custom';
  if (preset === 'best' && !$('original').checked) preset = 'custom';
  for (const b of document.querySelectorAll('[data-preset]')) b.setAttribute('aria-checked', String(b.dataset.preset === preset));
  fillFormats(); update();
});
$('format').addEventListener('change', update);
$('nas').addEventListener('change', update);

function fillFormats() {
  const sel = $('format');
  sel.innerHTML = '';
  sel.append(new Option('Automatic: best that fits the size', ''));
  for (const c of ladder()) {
    const extra = c.reencode_video ? ' · converts' : c.reencode_audio ? ' · converts audio' : '';
    sel.append(new Option(`${c.label} · ${size(c.size, !c.exact)}${extra}`, c.format));
  }
}

/** Mirrors the server's choice so the page shows what will be downloaded. */
function choose() {
  const list = ladder();
  const fmt = $('format').value;
  if (fmt) return list.find((c) => c.format === fmt);
  const effCap = cap == null ? null : Math.min(cap, me?.max_file_size ?? cap);
  const fit = list.find((c) => effCap == null || c.size == null || c.size <= effCap);
  if (fit) return fit;
  if (!probe.info.duration || !effCap) return null;
  const kbps = effCap * 8 / 1000 / probe.info.duration * 0.92 - 96;
  if (kbps < 80) return null;
  return { label: 'Re-encoded to fit', size: effCap, exact: false, reencode_video: true, reencode_audio: true, fit: true };
}
function estimate(c) {
  if (!c || c.size == null) return null;
  const r = probe.rates;
  const encode = (c.reencode_video || c.fit) ? (probe.info.duration || 0) * r.encode : 0;
  return c.size / r.speed + encode + 2;
}
function update() {
  if (!probe) return;
  const audio = mode() === 'audio';
  $('size-block').hidden = audio;
  $('size-out').textContent = cap == null ? 'No limit' : size(cap);
  const c = choose();
  const go = $('go');
  if (!c) {
    $('pick-label').textContent = 'Nothing fits under that size';
    $('pick-meta').textContent = 'Raise the limit, or pick a format under Advanced.';
    go.disabled = true;
    go.textContent = 'Download';
    return;
  }
  const label = $('pick-label');
  label.textContent = c.label + ' ';
  if (c.reencode_video || c.fit) {
    const w = document.createElement('span'); w.className = 'warn';
    w.textContent = c.fit ? '(shrunk to fit, takes longer)' : '(converted to H.264, takes longer)';
    label.append(w);
  }
  const t = estimate(c);
  $('pick-meta').textContent = [size(c.size, !c.exact), t != null ? `about ${dur(t)}` : null].filter(Boolean).join(' · ');
  go.disabled = false;
  go.textContent = $('nas').checked && me?.nas ? 'Save to NAS' : 'Download';
}

$('url-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  if (!probe) return runProbe();
  const go = $('go');
  go.disabled = true;
  try {
    const fmt = $('format').value;
    const body = {
      url: probe.url,
      mode: mode(),
      max_size: fmt ? null : cap,
      format: fmt || null,
      target: $('nas').checked && me?.nas ? 'nas' : 'browser',
    };
    const job = await api('/api/jobs', { method: 'POST', body: JSON.stringify(body) });
    mine.add(job.id);
    sessionStorage.setItem('mine', JSON.stringify([...mine]));
    upsert(job);
    $('url').value = '';
    resetProbe();
    $('url').focus();
  } catch (err) {
    showError($('probe-error'), err);
  } finally {
    go.disabled = false;
  }
});

// ---------- downloads list ----------
function connect() {
  events?.close();
  events = new EventSource(`/api/events?key=${encodeURIComponent(key)}`);
  events.onopen = () => $('live').classList.add('on');
  events.onerror = () => $('live').classList.remove('on');
  events.addEventListener('snapshot', (e) => {
    jobs.clear();
    for (const j of JSON.parse(e.data)) jobs.set(j.id, j);
    render();
  });
  events.addEventListener('job', (e) => upsert(JSON.parse(e.data)));
}
function upsert(j) {
  const prev = jobs.get(j.id);
  if (j.state === 'expired') jobs.delete(j.id); else jobs.set(j.id, j);
  if (j.state === 'ready' && prev?.state !== 'ready' && mine.has(j.id) && store.get('autosave', '1') === '1') {
    mine.delete(j.id);
    sessionStorage.setItem('mine', JSON.stringify([...mine]));
    saveFile(j.id);
  }
  render();
}
const fileUrl = (id) => `/api/jobs/${id}/file?key=${encodeURIComponent(key)}`;
function saveFile(id) {
  const a = document.createElement('a');
  a.href = fileUrl(id); a.download = '';
  document.body.append(a); a.click(); a.remove();
}

const LABEL = { queued: 'Queued', fetching: 'Downloading', processing: 'Processing', ready: 'Ready', saved: 'Saved', done: 'On NAS', failed: 'Failed', cancelled: 'Cancelled' };
function subline(j) {
  const p = j.progress;
  switch (j.state) {
    case 'queued': return 'In line, starts when a slot frees up';
    case 'fetching':
      if (!p.downloaded) return j.choice ? `${j.choice} · starting` : 'Getting info';
      return [
        p.total ? `${Math.floor(p.downloaded / p.total * 100)}%` : null,
        p.total ? `${size(p.downloaded)} of ${size(p.total)}` : size(p.downloaded),
        p.speed ? `${size(p.speed)}/s` : null,
        p.eta != null ? `${dur(p.eta)} left` : null,
      ].filter(Boolean).join(' · ');
    case 'processing': return p.processing != null ? `Converting · ${Math.floor(p.processing * 100)}%` : 'Finishing up';
    case 'ready': return [j.choice, size(j.size), 'ready to save'].filter(Boolean).join(' · ');
    case 'saved': return [j.choice, size(j.size), j.expires ? `kept until ${new Date(j.expires * 1000).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}` : null].filter(Boolean).join(' · ');
    case 'done': return [size(j.size), j.filename].filter(Boolean).join(' · ');
    case 'cancelled': return 'Cancelled';
    default: return '';
  }
}
function render() {
  const list = [...jobs.values()].sort((a, b) => b.created - a.created);
  $('empty').hidden = list.length > 0;
  const ul = $('jobs');
  const existing = new Map([...ul.children].map((li) => [li.dataset.id, li]));
  list.forEach((j, idx) => {
    let li = existing.get(j.id);
    if (!li) {
      li = $('job-tpl').content.firstElementChild.cloneNode(true);
      li.dataset.id = j.id;
      li.querySelector('.x').addEventListener('click', () => api(`/api/jobs/${j.id}`, { method: 'DELETE' }).catch(() => {}));
      li.querySelector('.fix-cookies').addEventListener('click', openSettings);
      li.querySelector('.retry').addEventListener('click', () => {
        $('url').value = jobs.get(j.id)?.url || ''; runProbe(); window.scrollTo({ top: 0, behavior: 'smooth' });
      });
    }
    existing.delete(j.id);
    if (ul.children[idx] !== li) ul.insertBefore(li, ul.children[idx] || null);

    const img = li.querySelector('img');
    if (j.thumbnail && img.getAttribute('src') !== j.thumbnail) img.src = j.thumbnail;
    li.querySelector('.thumb').hidden = !j.thumbnail;
    marquee(li.querySelector('.job-title'), j.title || j.url);
    li.querySelector('.job-title').title = j.url;
    const badge = li.querySelector('.badge');
    badge.className = `badge ${j.state}`;
    badge.textContent = (j.target === 'nas' && !['done', 'failed', 'cancelled'].includes(j.state) ? 'NAS · ' : '') + (LABEL[j.state] || j.state);
    li.querySelector('.job-sub').textContent = subline(j);

    const bar = li.querySelector('.progress');
    const fill = bar.firstElementChild;
    const active = ['queued', 'fetching', 'processing'].includes(j.state);
    bar.classList.toggle('hide', !active || j.state === 'queued');
    let frac = null;
    if (j.state === 'fetching' && j.progress.total) frac = j.progress.downloaded / j.progress.total;
    if (j.state === 'processing' && j.progress.processing != null) frac = j.progress.processing;
    bar.classList.toggle('indef', active && frac == null);
    fill.style.width = frac == null ? '' : `${Math.min(100, frac * 100)}%`;

    const err = li.querySelector('.job-err');
    err.hidden = j.state !== 'failed' || !j.error;
    if (j.error) {
      err.querySelector('.msg').textContent = j.error.message;
      err.querySelector('details').hidden = !j.error.detail;
      err.querySelector('code').textContent = j.error.detail || '';
    }
    li.querySelector('.fix-cookies').hidden = j.error?.hint !== 'cookies';
    const save = li.querySelector('.save');
    save.hidden = !['ready', 'saved'].includes(j.state);
    save.href = fileUrl(j.id);
    save.setAttribute('download', j.filename || '');
    save.textContent = j.state === 'saved' ? 'Save again' : 'Save';
    li.querySelector('.retry').hidden = !['failed', 'cancelled'].includes(j.state);
    li.querySelector('.x').title = active ? 'Cancel' : 'Remove from the list';
  });
  for (const li of existing.values()) li.remove();
}

// ---------- settings ----------
function openSettings() {
  $('settings-who').textContent = `Signed in as ${me.user}${me.nas ? ', with NAS access' : ''}.`;
  $('settings-key').value = key;
  $('autosave').checked = store.get('autosave', '1') === '1';
  $('cookies-msg').hidden = true;
  $('cookies-file').value = '';
  cookieStatus();
  extSettings('get-settings');
  $('settings').showModal();
}

// ---------- the Firefox extension, when installed, exposes its settings here ----------
let extState = null;
function extSettings(type, settings) {
  if (document.documentElement.dataset.mdExtension !== '1') return;
  window.postMessage({ source: 'md-page', type, settings }, location.origin);
}
window.addEventListener('message', (e) => {
  if (e.source !== window || e.data?.source !== 'md-extension' || e.data.type !== 'settings' || !e.data.settings) return;
  extState = e.data.settings;
  $('ext-settings').hidden = false;
  $('ext-preset').value = `${extState.videoMode}:${extState.videoCap}`;
  $('ext-notify').checked = !!extState.notify;
});
$('who').addEventListener('click', openSettings);
function cookieStatus() {
  const when = me.cookies ? new Date(me.cookies * 1000).toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' }) : null;
  $('cookies-status').textContent = when ? `Using your cookies, uploaded ${when}.` : 'Not uploaded. Your downloads use the shared account.';
  $('cookies-remove').hidden = !when;
}
function cookieMsg(text, bad) {
  $('cookies-msg').textContent = text;
  $('cookies-msg').className = bad ? 'small error' : 'small muted';
  $('cookies-msg').hidden = false;
}
$('cookies-upload').addEventListener('click', async () => {
  const file = $('cookies-file').files[0];
  if (!file) return cookieMsg('Choose your cookies.txt file first.', true);
  try {
    const r = await api('/api/cookies', { method: 'POST', body: await file.text(), headers: { 'Content-Type': 'text/plain' } });
    me.cookies = r.cookies;
    cookieStatus();
    cookieMsg(`Uploaded ${r.entries} cookies.`);
  } catch (err) { cookieMsg(err.detail ? `${err.message} ${err.detail}` : err.message, true); }
});
$('cookies-remove').addEventListener('click', async () => {
  try { await api('/api/cookies', { method: 'DELETE' }); me.cookies = null; cookieStatus(); cookieMsg('Removed.'); }
  catch (err) { cookieMsg(err.message, true); }
});
$('settings').addEventListener('close', async () => {
  const v = $('settings').returnValue;
  if (v === 'forget') return signOut();
  if (v !== 'save') return;
  store.set('autosave', $('autosave').checked ? '1' : '0');
  if (extState) {
    const [videoMode, cap] = $('ext-preset').value.split(':');
    extSettings('set-settings', { videoMode, videoCap: +cap, notify: $('ext-notify').checked });
  }
  const k = parseKey($('settings-key').value);
  if (k && k !== key) {
    const old = key;
    try { await signIn(k); } catch { key = old; alert("That key isn't valid; kept the old one."); }
  }
});

// ---------- start ----------
// The extension link shows once a signed build has been dropped into web/.
if (navigator.userAgent.includes('Firefox')) {
  fetch('media-downloader.xpi', { method: 'HEAD' }).then((r) => { $('ext').hidden = !r.ok || !(r.headers.get('content-type') || '').includes('xpinstall'); }).catch(() => {});
}

// Links like /?url=... (from the extension or a bookmarklet) prefill the box.
const startUrl = new URLSearchParams(location.search).get('url');
if (key) {
  signIn(key).then(() => {
    if (startUrl) { $('url').value = startUrl; runProbe(); }
    if (new URLSearchParams(location.search).has('settings')) {
      openSettings();
      $('cookies-section').scrollIntoView({ block: 'nearest' });
    }
  }).catch(() => {});
} else {
  signOut();
}
