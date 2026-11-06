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
load();
