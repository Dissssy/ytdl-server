'use strict';
const $ = (id) => document.getElementById(id);
const DEFAULTS = { server: 'https://ytdlp.deadlyneurotox.in', key: '', videoMode: 'compat', videoCap: 0, notify: true };

async function load() {
  const s = { ...DEFAULTS, ...(await browser.storage.local.get(Object.keys(DEFAULTS))) };
  $('server').value = s.server;
  $('key').value = s.key;
  $('preset').value = `${s.videoMode}:${s.videoCap}`;
  $('notify').checked = s.notify;
}

$('save').onclick = async () => {
  let key = $('key').value.trim();
  try { key = new URL(key).searchParams.get('key') || key; } catch {}
  const [videoMode, cap] = $('preset').value.split(':');
  const server = $('server').value.trim().replace(/\/+$/, '');
  $('status').textContent = 'Checking…';
  try {
    const res = await fetch(`${server}/api/me`, { headers: { Authorization: `Bearer ${key}` } });
    if (!res.ok) throw new Error(res.status === 401 ? "That key isn't valid." : `Server answered ${res.status}.`);
    const me = await res.json();
    await browser.storage.local.set({ server, key, videoMode, videoCap: +cap, notify: $('notify').checked });
    $('status').textContent = `Saved. Signed in as ${me.user}${me.nas ? ', downloads go to the NAS' : ''}.`;
  } catch (e) {
    $('status').textContent = e.message || "Couldn't reach the server.";
  }
};
// ---------- cookies: uploaded only when the button is pressed ----------
async function cookieApi(method, body) {
  const s = { ...DEFAULTS, ...(await browser.storage.local.get(['server', 'key'])) };
  const res = await fetch(`${s.server}${method === 'GET' ? '/api/me' : '/api/cookies'}`, {
    method, body, headers: { Authorization: `Bearer ${s.key}`, ...(body ? { 'Content-Type': 'text/plain' } : {}) },
  });
  const text = await res.text();
  let data = null; try { data = JSON.parse(text); } catch {}
  if (!res.ok) throw new Error([data?.message, data?.detail].filter(Boolean).join(' ') || `Server answered ${res.status}.`);
  return data;
}
async function cookieStatus() {
  try {
    const me = await cookieApi('GET');
    $('cookies-status').textContent = me.cookies
      ? `Using your cookies, uploaded ${new Date(me.cookies * 1000).toLocaleDateString()}.`
      : 'Not uploaded. Your downloads use the shared account.';
    $('cookies-remove').hidden = !me.cookies;
  } catch { $('cookies-status').textContent = 'Sign in above first.'; }
}
$('cookies-upload').onclick = async () => {
  const file = $('cookies-file').files[0];
  if (!file) { $('cookies-msg').textContent = 'Choose your cookies.txt file first.'; return; }
  try {
    const r = await cookieApi('POST', await file.text());
    $('cookies-msg').textContent = `Uploaded ${r.entries} cookies.`;
    $('cookies-file').value = '';
  } catch (e) { $('cookies-msg').textContent = e.message; }
  cookieStatus();
};
$('cookies-remove').onclick = async () => {
  try { await cookieApi('DELETE'); $('cookies-msg').textContent = 'Removed.'; } catch (e) { $('cookies-msg').textContent = e.message; }
  cookieStatus();
};
load();
cookieStatus();
