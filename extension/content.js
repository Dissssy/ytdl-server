'use strict';
// Runs on every page: exposes the Gesturify functions, and shows a small progress toast.

// Gesturify calls window.dnrNAMESPACE.download_video(close, src) / download_audio(...).
(() => {
  const page = window.wrappedJSObject;
  if (!page || typeof exportFunction !== 'function') return;
  const ns = page.dnrNAMESPACE || createObjectIn(page, { defineAs: 'dnrNAMESPACE' });
  const send = (audio) => (close, src) => {
    browser.runtime.sendMessage({ type: 'download', audio, close: !!close, url: src || location.href });
  };
  exportFunction(send(false), ns, { defineAs: 'download_video' });
  exportFunction(send(true), ns, { defineAs: 'download_audio' });
})();

// On the downloader's own site: hand its stored key to the extension on every visit, and let
// the site's Settings dialog read and change the extension's settings.
if (document.querySelector('script[src="app.js"]')) {
  const syncKey = () => {
    try {
      const key = localStorage.getItem('api_key');
      if (key) browser.runtime.sendMessage({ type: 'site-key', key });
    } catch {}
  };
  syncKey();
  document.documentElement.dataset.mdExtension = '1';
  window.addEventListener('message', async (e) => {
    if (e.source !== window || e.data?.source !== 'md-page') return;
    if (e.data.type === 'signed-in') return syncKey();
    if (e.data.type === 'upload-cookies') {
      const result = await browser.runtime.sendMessage({ type: 'upload-cookies', domains: e.data.domains });
      window.postMessage({ source: 'md-extension', type: 'cookies-uploaded', result }, location.origin);
      return;
    }
    if (e.data.type === 'get-settings' || e.data.type === 'set-settings') {
      const settings = await browser.runtime.sendMessage({ type: e.data.type, settings: e.data.settings });
      window.postMessage({ source: 'md-extension', type: 'settings', settings: settings || null }, location.origin);
    }
  });
}

// ---------- toast ----------
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

let host = null, box = null;
const toasts = new Map();
const fmt = (b) => b == null ? '' : b < 1048576 ? `${Math.round(b / 1024)} KB` : `${(b / 1048576).toFixed(1)} MB`;
const LABEL = { queued: 'Queued', fetching: 'Downloading', processing: 'Processing', ready: 'Saving to Downloads', saved: 'Saved', done: 'Saved to NAS', failed: 'Failed', cancelled: 'Cancelled' };

function ensure() {
  if (host?.isConnected) return;
  host = document.createElement('div');
  host.style.cssText = 'all: initial; position: fixed; z-index: 2147483647; right: 16px; bottom: 16px;';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = `<style>
    :host { all: initial; }
    .wrap { display: grid; grid-template-columns: minmax(0, 1fr); gap: 8px; font: 13px/1.4 system-ui, sans-serif; width: min(320px, calc(100vw - 32px)); }
    .t { min-width: 0; background: #18181e; color: #ececf1; border-radius: 12px; padding: 10px 12px; box-shadow: 0 6px 24px #0005; }
    .top { display: flex; justify-content: space-between; gap: 8px; }
    .title { font-weight: 600; flex: 1; }
    .top { min-width: 0; }
.marquee { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; min-width: 0; }
    .marquee.run { text-overflow: clip; }
    .marquee.run > span { display: inline-block; animation: marquee var(--dur) ease-in-out infinite alternate; }
    @keyframes marquee { 0%, 20% { transform: translateX(0); } 80%, 100% { transform: translateX(calc(-1 * var(--shift))); } }
    @media (prefers-reduced-motion: reduce) { .marquee.run { text-overflow: ellipsis; } .marquee.run > span { display: inline; animation: none; } }
    .x { background: none; border: 0; color: #9a9aa8; cursor: pointer; font: inherit; padding: 0 2px; }
    .sub { color: #9a9aa8; margin-top: 2px; }
    .sub.err { color: #ff6b6b; }
    .fix { margin-top: 6px; background: #8b7dff; color: #0f0f13; border: 0; border-radius: 6px; padding: 4px 10px; font: 600 12px system-ui, sans-serif; cursor: pointer; }
    [hidden] { display: none !important; }
    .bar { height: 4px; background: #2a2a33; border-radius: 2px; overflow: hidden; margin-top: 6px; }
    .bar > div { height: 100%; background: #8b7dff; width: 0; transition: width .4s; }
  </style><div class="wrap"></div>`;
  box = root.querySelector('.wrap');
  document.documentElement.append(host);
}

browser.runtime.onMessage.addListener((msg) => {
  if (msg.type !== 'toast') return;
  const j = msg.job;
  ensure();
  let el = toasts.get(j.id);
  if (!el) {
    el = document.createElement('div');
    el.className = 't';
    el.innerHTML = '<div class="top"><span class="title"></span><button class="x" title="Dismiss">✕</button></div><div class="sub"></div><button class="fix" hidden>Upload cookies</button><div class="bar"><div></div></div>';
    el.querySelector('.fix').onclick = () => browser.runtime.sendMessage({ type: 'open-site', path: '?settings=cookies' });
    el.querySelector('.x').onclick = () => { el.remove(); toasts.delete(j.id); };
    box.append(el);
    toasts.set(j.id, el);
  }
  marquee(el.querySelector('.title'), j.title || j.url || 'Download');
  const p = j.progress || {};
  let frac = null, sub = LABEL[j.state] || j.state;
  if (j.state === 'fetching' && p.total) { frac = p.downloaded / p.total; sub = `${Math.floor(frac * 100)}% · ${fmt(p.downloaded)} of ${fmt(p.total)}`; }
  if (j.state === 'processing' && p.processing != null) { frac = p.processing; sub = `Converting · ${Math.floor(frac * 100)}%`; }
  if (j.state === 'failed') sub = j.error?.message || 'Failed';
  const fix = el.querySelector('.fix');
  fix.hidden = !(j.state === 'failed' && j.error?.hint === 'cookies');
  if (j.target === 'nas' && ['queued', 'fetching', 'processing'].includes(j.state)) sub = 'NAS · ' + sub;
  const subEl = el.querySelector('.sub');
  subEl.textContent = sub;
  subEl.className = j.state === 'failed' ? 'sub err' : 'sub';
  const active = ['queued', 'fetching', 'processing'].includes(j.state);
  el.querySelector('.bar').style.display = active ? '' : 'none';
  el.querySelector('.bar > div').style.width = frac == null ? '15%' : `${frac * 100}%`;
  if (!active) setTimeout(() => { el.remove(); toasts.delete(j.id); }, j.state === 'failed' ? 12000 : 4000);
});
