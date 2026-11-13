'use strict';
/* StreamTranslator UI. Talks to Python through QWebChannel: api(method, params) -> Promise,
   and on(eventType, fn) for progress/live events pushed from the backend. */

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
function h(tag, attrs = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'style') el.style.cssText = v;
    else if (k === 'html') el.innerHTML = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const k of kids.flat()) if (k != null && k !== false) el.append(k.nodeType ? k : document.createTextNode(k));
  return el;
}
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
function debounce(fn, ms) { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; }

/* ---------- time ---------- */
function fmt(s) {
  s = Math.max(0, Math.round(s)); const H = Math.floor(s / 3600), M = Math.floor(s % 3600 / 60), X = s % 60;
  return H ? `${H}:${String(M).padStart(2, '0')}:${String(X).padStart(2, '0')}` : `${M}:${String(X).padStart(2, '0')}`;
}
function parseTs(t) {
  t = (t || '').trim(); if (!t) return null;
  if (t.includes(':')) { const p = t.split(':'); if (p.length > 3 || p.some(x => x.trim() === '' || isNaN(+x))) return null; return p.reduce((a, x) => a * 60 + +x, 0); }
  if (!isNaN(+t)) return +t;
  const m = t.match(/^(?:(\d+)h)?\s*(?:(\d+)m)?\s*(?:(\d+(?:\.\d+)?)s)?$/i);
  return m && (m[1] || m[2] || m[3]) ? (+m[1] || 0) * 3600 + (+m[2] || 0) * 60 + (+m[3] || 0) : null;
}
const hmsTag = s => { s = Math.floor(s); return `${Math.floor(s / 3600)}h${String(Math.floor(s % 3600 / 60)).padStart(2, '0')}m${String(s % 60).padStart(2, '0')}s`; };
function ago(ts) { const d = Date.now() / 1000 - ts; if (d < 3600) return Math.max(1, Math.round(d / 60)) + ' min ago'; if (d < 86400) return Math.round(d / 3600) + ' h ago'; const n = Math.round(d / 86400); return n === 1 ? 'yesterday' : n + ' days ago'; }
const durTxt = s => { const H = Math.floor(s / 3600), M = Math.floor(s % 3600 / 60); return H ? `${H}h ${M}m` : `${M}m`; };
const spanTxt = s => { s = Math.round(Math.abs(s)); const H = Math.floor(s / 3600), M = Math.floor(s % 3600 / 60), X = s % 60; return (H ? H + 'h ' : '') + (H || M ? M + 'm ' : '') + X + 's'; };

const LANGS = { ar: 'Arabic', bg: 'Bulgarian', cs: 'Czech', da: 'Danish', de: 'German', el: 'Greek', en: 'English', es: 'Spanish', fi: 'Finnish', fr: 'French', he: 'Hebrew', hi: 'Hindi', hr: 'Croatian', hu: 'Hungarian', id: 'Indonesian', it: 'Italian', ja: 'Japanese', ko: 'Korean', lt: 'Lithuanian', ms: 'Malay', nl: 'Dutch', no: 'Norwegian', pl: 'Polish', pt: 'Portuguese', ro: 'Romanian', ru: 'Russian', sk: 'Slovak', sr: 'Serbian', sv: 'Swedish', th: 'Thai', tl: 'Tagalog', tr: 'Turkish', uk: 'Ukrainian', vi: 'Vietnamese', zh: 'Chinese' };

/* ---------- bridge ---------- */
let backend; let seq = 0; const pending = new Map(); const handlers = {};
function api(method, params = {}) {
  return new Promise((res, rej) => { const id = ++seq; pending.set(id, { res, rej }); backend.request(JSON.stringify({ id, method, params })); });
}
function on(type, fn) { (handlers[type] = handlers[type] || []).push(fn); }
function dispatch(raw) {
  const m = JSON.parse(raw);
  if (m.type === 'reply') { const p = pending.get(m.id); if (p) { pending.delete(m.id); m.ok ? p.res(m.result) : p.rej(new Error(m.error)); } return; }
  (handlers[m.type] || []).forEach(f => { try { f(m); } catch (e) { console.error(e); } });
}

/* ---------- small components ---------- */
function toast(msg, kind = '', ms = 4500) {
  const t = h('div', { class: 'toast ' + kind }, msg); $('#toasts').append(t);
  setTimeout(() => { t.classList.add('out'); setTimeout(() => t.remove(), 300); }, ms);
}
function seg(el, onChange) {
  const thumb = h('div', { class: 'thumb' }); el.prepend(thumb);
  const key = b => b.dataset.v ?? b.dataset.page;
  const move = () => { const b = el.querySelector('button.on'); if (!b) { thumb.style.width = '0'; return; } if (!b.offsetWidth) return; thumb.style.left = b.offsetLeft + 'px'; thumb.style.width = b.offsetWidth + 'px'; };
  const set = v => { $$('button', el).forEach(b => b.classList.toggle('on', key(b) === v)); move(); };
  el.addEventListener('click', e => { const b = e.target.closest('button'); if (!b || b.disabled || b.classList.contains('on')) return; set(key(b)); onChange && onChange(key(b)); });
  new ResizeObserver(move).observe(el); requestAnimationFrame(move);
  return { set, move, get: () => { const b = el.querySelector('button.on'); return b && key(b); } };
}
function champIcon(c) { return c.icon ? h('img', { src: c.icon, alt: '', loading: 'lazy' }) : h('span', { class: 'ini' }, c.name.slice(0, 2)); }
function chipInput(root, { free = false, placeholder = '', onChange } = {}) {
  const input = h('input', { placeholder }); root.append(input);
  const chosen = []; let ac = null, list = [], idx = 0;
  const champ = n => (S.champs.find(c => c.name === n) || { name: n });
  const render = () => {
    $$('.chip', root).forEach(x => x.remove());
    chosen.forEach(n => root.insertBefore(h('span', { class: 'chip' }, free ? null : champIcon(champ(n)), n,
      h('button', { title: 'Remove', onclick: e => { e.stopPropagation(); remove(n); } }, '×')), input));
    input.placeholder = chosen.length ? '' : placeholder;
  };
  const changed = () => { render(); onChange && onChange(chosen.slice()); };
  const add = n => { n = n.trim(); if (n && !chosen.some(x => x.toLowerCase() === n.toLowerCase()) && (free || chosen.length < 10)) { chosen.push(n); changed(); } input.value = ''; close(); };
  const remove = n => { const i = chosen.indexOf(n); if (i >= 0) { chosen.splice(i, 1); changed(); } };
  const close = () => { ac && ac.remove(); ac = null; };
  const open = () => {
    if (free) return;
    const q = input.value.trim().toLowerCase().replace(/[^a-z]/g, ''); if (!q) return close();
    list = S.champs.filter(c => !chosen.includes(c.name) && c.name.toLowerCase().replace(/[^a-z]/g, '').includes(q))
      .sort((a, b) => b.name.toLowerCase().startsWith(q) - a.name.toLowerCase().startsWith(q)).slice(0, 8);
    if (!list.length) return close();
    idx = 0; if (!ac) { ac = h('div', { class: 'ac' }); root.append(ac); }
    ac.innerHTML = ''; list.forEach((c, i) => ac.append(h('button', { class: i === idx ? 'on' : '', onmousedown: e => { e.preventDefault(); add(c.name); } }, champIcon(c), c.name)));
  };
  input.addEventListener('input', () => { if (free && input.value.includes(',')) { input.value.split(',').forEach(add); return; } open(); });
  input.addEventListener('keydown', e => {
    if ((e.key === 'ArrowDown' || e.key === 'ArrowUp') && ac) { e.preventDefault(); idx = (idx + (e.key === 'ArrowDown' ? 1 : -1) + list.length) % list.length; $$('button', ac).forEach((b, i) => b.classList.toggle('on', i === idx)); }
    else if (e.key === 'Enter' || (e.key === 'Tab' && ac)) { if (free && input.value.trim()) { e.preventDefault(); add(input.value); } else if (ac && list[idx]) { e.preventDefault(); add(list[idx].name); } }
    else if (e.key === 'Backspace' && !input.value && chosen.length) remove(chosen[chosen.length - 1]);
    else if (e.key === 'Escape') close();
  });
  input.addEventListener('blur', () => { setTimeout(close, 120); if (free && input.value.trim()) add(input.value); });
  root.addEventListener('click', () => input.focus());
  return { get: () => chosen.slice(), set: arr => { chosen.length = 0; chosen.push(...(arr || [])); render(); } };
}

/* ---------- state ---------- */
const S = {
  settings: {}, gpu: '', champs: [], gloss: null,
  vod: null, tl: { dur: 1, v0: 0, v1: 1, lo: 0, hi: 1, active: 0 },
  sync: null, job: null, model: { name: 'large-v3', state: 'idle' },
  live: { on: false, started: 0, lines: 0, transcript: null, timer: null },
};
const saveSettings = debounce(() => api('settings.save', { data: S.settings }).catch(() => { }), 400);
function setSetting(k, v) { S.settings[k] = v; saveSettings(); }

/* ---------- boot ---------- */
window.addEventListener('DOMContentLoaded', () => {
  new QWebChannel(qt.webChannelTransport, ch => { backend = ch.objects.backend; backend.event.connect(dispatch); boot(); });
});
let methodSeg, liveMethod, trSeg;
function syncTurbo() {
  // Turbo can only write down speech, not translate it, so it needs the two-step method
  const whisperOnly = methodSeg.get() === 'whisper';
  const turbo = $('#modelSeg [data-v="large-v3-turbo"]'); turbo.disabled = whisperOnly; turbo.title = whisperOnly ? 'Turbo needs the two-step method' : 'About 3x faster, slightly less accurate';
  if (whisperOnly && modelSeg.get() === 'large-v3-turbo') { modelSeg.set('large-v3'); setSetting('model', 'large-v3'); }
  $('#methodHint').textContent = whisperOnly ? 'Whisper translates directly (the original method)' : 'Whisper writes it down, then a translator turns it into English';
}
let nav, srcMode, modelSeg, qualSeg, sizeSeg, liveModel, champsClip, champsLive, customWords, syncTheirs;

async function boot() {
  nav = seg($('#nav'), showPage);
  const r = await api('app.init');
  S.settings = r.settings; S.gpu = r.gpu || ''; S.scale = r.scale; S.update = r.update || {}; S.appUpdate = r.app_update || {};
  applyGloss(r.glossary);
  initClip(); initLive(); initGlossary(); initSettings(); initHistory(); initEditor();
  initSetup(r.components, r.setup_needed && !r.settings.setup_done || (r.components && !r.components.gpu.ok && r.components.has_nvidia));
  if (r.queue && r.queue.length) renderQueue(r.queue);
  if (r.whats_new) showWhatsNew(r.whats_new);
  if (r.crashed) actionToast('StreamTranslator closed unexpectedly last time. Sending a report helps get it fixed.', 'warn',
    [['Report', () => reportProblem('The app closed unexpectedly')], ['Open logs', () => api('app.logs')]]);
  if (!r.ffmpeg) toast('ffmpeg was not found. Install it with:  winget install Gyan.FFmpeg', 'bad', 15000);
  S.model.name = r.model.name;
  modelChip(r.model.device ? { state: 'ready', name: r.model.name, device: r.model.device } : { state: S.settings.preload ? 'loading' : 'idle', name: r.model.name });
  checkClipboard();
  window.addEventListener('focus', checkClipboard);
  document.addEventListener('keydown', globalKeys);
  initDrop();
}
function showPage(p) { $$('.page').forEach(x => x.classList.toggle('on', x.id === 'page-' + p)); nav.set(p); if (p === 'history') loadHistory(); }
function globalKeys(e) {
  if (!$('#editor').hidden) return;
  if (e.ctrlKey && e.key >= '1' && e.key <= '5') { e.preventDefault(); showPage(['clip', 'live', 'history', 'glossary', 'settings'][+e.key - 1]); }
  if (e.ctrlKey && e.key === 'Enter' && $('#page-clip').classList.contains('on') && !$('#goBtn').disabled) startClip();
}

/* ---------- model chip ---------- */
on('model.status', modelChip);
function modelChip(m) {
  S.model = { ...S.model, ...m };
  const c = $('#modelChip');
  c.className = 'status-chip ' + ({ downloading: 'loading', loading: 'loading', ready: 'ready', error: 'error' }[m.state] || '');
  const gpu = S.gpu.replace(/^NVIDIA (GeForce )?/, '');
  $('.txt', c).textContent = {
    ready: `Whisper ${m.name} · ready on ${m.device === 'GPU' ? (gpu || 'GPU') : 'CPU'}`,
    downloading: `Downloading Whisper ${m.name} (one time, ~3 GB)…`,
    loading: `Warming up Whisper ${m.name}…`,
    error: 'Whisper failed to load',
  }[m.state] || `Whisper ${m.name} · loads on first use`;
  if (m.state === 'error') toast('Whisper failed to load:\n' + (m.error || ''), 'bad', 9000);
  renderAbout();
}

/* ================================================================ CLIP */
const TL = {};
function initClip() {
  srcMode = seg($('#srcMode'), v => $$('#sourceCard .mode-pane').forEach(p => p.classList.toggle('on', p.dataset.mode === v)));
  $('#theirLoad').onclick = () => loadSource($('#theirInput').value);
  $('#theirFile').onclick = pickVideo;
  on('files.dropped', m => { const f = (m.paths || [])[0]; if (f) useVideoFile(f); });
  $('#theirInput').addEventListener('keydown', e => { if (e.key === 'Enter') loadSource(e.target.value); });
  $('#theirInput').addEventListener('paste', () => setTimeout(() => { const v = $('#theirInput').value.trim(); if (VOD_RX.test(v) || /^\d{8,}$/.test(v) || isLocal(v)) loadSource(v); }, 30));

  // options
  const st = S.settings;
  const langOpts = sel => { sel.append(h('option', { value: '' }, 'Auto-detect')); Object.entries(LANGS).sort((a, b) => a[1].localeCompare(b[1])).forEach(([c, n]) => sel.append(h('option', { value: c }, n))); };
  langOpts($('#langSel')); langOpts($('#liveLang'));
  $('#langSel').value = st.language || ''; $('#langSel').onchange = e => setSetting('language', e.target.value);
  modelSeg = seg($('#modelSeg'), v => { setSetting('model', v); api('model.preload', { name: v }).catch(() => { }); }); modelSeg.set(st.model);
  qualSeg = seg($('#qualSeg'), v => setSetting('quality', v)); qualSeg.set(st.quality);
  sizeSeg = seg($('#sizeSeg'), v => setSetting('size', v)); sizeSeg.set(st.size);
  $$('#posPick button').forEach(b => { b.classList.toggle('on', b.dataset.v === st.position); b.onclick = () => { $$('#posPick button').forEach(x => x.classList.toggle('on', x === b)); setSetting('position', b.dataset.v); }; });
  $('#burnTog').checked = st.burn; $('#burnTog').onchange = e => setSetting('burn', e.target.checked);
  $('#origTog').checked = st.orig; $('#origTog').onchange = e => setSetting('orig', e.target.checked);
  $('#glossClip').checked = st.use_glossary; $('#glossClip').onchange = e => setSetting('use_glossary', e.target.checked);
  methodSeg = seg($('#methodSeg'), v => { setSetting('method', v); syncTurbo(); }); methodSeg.set(st.method || 'two-step');
  $('#autoClip').checked = st.auto_details; $('#autoClip').onchange = e => setSetting('auto_details', e.target.checked);
  $('#spkClip').checked = !!st.speakers; $('#spkClip').onchange = e => setSetting('speakers', e.target.checked);
  syncTurbo();
  $('#isoClip').checked = st.isolate; $('#isoClip').onchange = e => setSetting('isolate', e.target.checked);
  $('#snapBtn').onclick = runSnap;
  $('#queueClear').onclick = () => api('queue.clear');
  syncTheirs = chipInput($('#syncTheirs'), { free: true, placeholder: 'ibai, caedrel, or VOD links… press Enter after each' });
  champsClip = chipInput($('#champsClip'), { placeholder: 'Type a champion…' });
  $('#outDir').value = st.out_dir; $('#browseBtn').onclick = browseOut;
  $('#goBtn').onclick = startClip;
  $('#cancelBtn').onclick = () => api('clip.cancel');
  $$('#results .res').forEach(b => b.onclick = () => {
    const r = S.lastResult; if (!r) return;
    if (b.dataset.open === 'edit') return openEditor(r.folder, r.channel);
    api('os.open', { path: { subbed: r.subbed || r.clip, srt: r.srt, transcript: r.transcript, folder: r.folder }[b.dataset.open] });
  });

  // sync
  $('#myVodsBtn').onclick = myVods;
  $('#syncGo').onclick = runSync;
  ['#myStart', '#myEnd'].forEach(s => $(s).addEventListener('keydown', e => { if (e.key === 'Enter') runSync(); }));
  document.addEventListener('click', e => { if (!e.target.closest('#myVodList,#myVodsBtn')) $('#myVodList').hidden = true; });

  // timeline
  Object.assign(TL, { el: $('#tl'), track: $('#tlTrack'), film: $('#tlFilm'), sel: $('#tlSel'), ml: $('#tlMaskL'), mr: $('#tlMaskR'), hs: $$('.tl-handle'), ticks: $('#tlTicks'), pop: $('#tlPop'), cursor: $('#tlCursor'), el1: $('#tlEdgeL'), el2: $('#tlEdgeR') });
  initTimeline();
  $('#vodOpen').onclick = () => S.vod && api('os.url', { url: twitchAt(S.tl.lo) });
}

async function checkClipboard() {
  try {
    const txt = (await api('os.clipboard')) || '';
    const m = txt.match(VOD_RX);
    const hint = $('#clipHint');
    if (!m || m[0] === S.clipSeen || (S.vod && m[0].includes(S.vod.id))) return;
    hint.innerHTML = ''; hint.append(h('span', {}, 'Link on your clipboard:'), h('b', {}, m[0].replace(/^https?:\/\/(www\.)?/, '')),
      h('button', { onclick: () => { $('#theirInput').value = m[0]; hint.hidden = true; srcMode.set('theirs'); $$('#sourceCard .mode-pane').forEach(p => p.classList.toggle('on', p.dataset.mode === 'theirs')); loadSource(m[0]); } }, 'Use it'));
    hint.hidden = false; S.clipSeen = m[0];
  } catch { }
}

async function loadSource(text) {
  text = (text || '').trim(); if (!text) { $('#theirInput').focus(); return; }
  const btn = $('#theirLoad'); btn.classList.add('busy'); $('#clipHint').hidden = true;
  try {
    const r = await api('source.load', { text });
    if (r.kind === 'channel') vodGrid($('#vodGrid'), r.login, r.vods, v => loadSource(v.url));
    else { $('#vodGrid').innerHTML = ''; setVod(r.vod); }
  } catch (e) { toast(e.message, 'bad', 8000); }
  finally { btn.classList.remove('busy'); }
}
/* ---------- video files on this PC ---------- */
const isLocal = u => /^"?([a-z]:[\\/]|\\\\|file:\/\/\/)/i.test((u || '').trim());
async function pickVideo() {
  try { const f = await api('os.pick_video'); if (f) useVideoFile(f); } catch (e) { toast(e.message, 'bad', 6000); }
}
function useVideoFile(path) {
  if ($('#page-live').classList.contains('on')) { toast('Live mode needs a channel. Video files go in the Clip tab.', 'bad', 5000); return; }
  showPage('clip'); srcMode.set('theirs'); $$('#sourceCard .mode-pane').forEach(p => p.classList.toggle('on', p.dataset.mode === 'theirs'));
  $('#theirInput').value = path; loadSource(path);
}
async function loadStoryboard(v) {
  if (v.site !== 'local' || v.storyboard) return;
  try { const sb = await api('vod.storyboard', { id: v.id }); if (S.vod && S.vod.id === v.id) { S.vod.storyboard = sb; renderFilm(); } } catch { }
}
function vodGrid(root, login, vods, pick) {
  root.innerHTML = '';
  vods.forEach((v, i) => root.append(h('button', { class: 'vod-card', style: `animation-delay:${i * 35}ms`, onclick: () => pick(v) },
    h('div', { class: 'th', style: v.thumbnail ? `background-image:url("${v.thumbnail}")` : '' }, i === 0 ? h('em', {}, 'LATEST') : null, h('span', {}, durTxt(v.duration))),
    h('div', { class: 'tt' }, h('b', {}, v.title), h('span', {}, `${login} · ${v.start_ts ? ago(v.start_ts) : ''}`)))));
}

function setVod(v, range) {
  S.vod = v; S.frameCache = S.frameCache || new Map();
  const card = $('#sectionCard'); card.hidden = false; card.classList.remove('reveal'); void card.offsetWidth; card.classList.add('reveal');
  $('#vodThumb').src = v.thumbnail || ''; $('#vodTitle').textContent = v.title;
  $('#vodSub').textContent = `${v.channel} · ${durTxt(v.duration)}${v.start_ts ? ' · streamed ' + ago(v.start_ts) : ''}`;
  const d = v.duration || 1;
  let lo, hi;
  if (range) [lo, hi] = range;
  else if (v.url_start != null) { lo = v.url_start; hi = Math.min(d, lo + 1800); }
  else { lo = 0; hi = Math.min(d, 1800); }
  Object.assign(S.tl, { dur: d, v0: 0, v1: d, lo, hi, active: 0 });
  requestAnimationFrame(() => { renderFilm(); renderTicks(); renderTL(); if (hi - lo < d * 0.08) zoomSel(); });
  $('#vodOpen').title = v.site === 'local' ? 'Open the video file' : 'Open the VOD at the start';
  loadStoryboard(v);
  commitRange(true);
  $('#goBtn').disabled = false;
  setTimeout(() => card.scrollIntoView({ behavior: 'smooth', block: 'start' }), 120);
  if (v.url_start != null && !range) toast(`Start set from the link: ${fmt(v.url_start)}`, 'good', 3000);
}
const twitchAt = t => S.vod ? vodAt(S.vod.url, S.vod.id, t) : '';

/* ---------- timeline ---------- */
const xOf = t => (t - S.tl.v0) / (S.tl.v1 - S.tl.v0) * 100;
function tAt(clientX) { const r = TL.track.getBoundingClientRect(); return clamp(S.tl.v0 + (clientX - r.left) / r.width * (S.tl.v1 - S.tl.v0), 0, S.tl.dur); }
let rafPending = false;
function schedule() { if (!rafPending) { rafPending = true; requestAnimationFrame(() => { rafPending = false; renderTL(); }); } }
function renderTL() {
  const { lo, hi, v0, v1, active } = S.tl;
  const a = clamp(xOf(lo), 0, 100), b = clamp(xOf(hi), 0, 100);
  Object.assign(TL.sel.style, { left: a + '%', width: Math.max(0, b - a) + '%' });
  Object.assign(TL.ml.style, { left: 0, width: a + '%' });
  Object.assign(TL.mr.style, { left: b + '%', right: 0 });
  [lo, hi].forEach((t, i) => { const hd = TL.hs[i]; hd.style.left = xOf(t) + '%'; hd.style.display = t < v0 - .5 || t > v1 + .5 ? 'none' : ''; hd.classList.toggle('active', i === active && document.activeElement === TL.el); });
  TL.el1.classList.toggle('on', v0 > 0.5); TL.el2.classList.toggle('on', v1 < S.tl.dur - 0.5);
  if (document.activeElement !== $('#startIn')) $('#startIn').value = fmt(lo);
  if (document.activeElement !== $('#endIn')) $('#endIn').value = fmt(hi);
  $('#lenPill').textContent = fmt(hi - lo);
  $('#frameStart .badge b').textContent = fmt(lo); $('#frameEnd .badge b').textContent = fmt(hi);
  const mp = $('#mapPill');
  if (S.sync && S.vod && S.sync.theirId === S.vod.id) { mp.hidden = false; mp.innerHTML = `your VOD<br><b>${fmt(lo - S.sync.offset)} – ${fmt(hi - S.sync.offset)}</b>`; }
  else mp.hidden = true;
}
function sprite(el, t, w, hh) {
  const sb = S.vod && S.vod.storyboard; if (!sb) return el;
  const per = sb.cols * sb.rows; const idx = Math.min(Math.floor(t / sb.interval), sb.sheets.length * per - 1);
  const k = idx % per; const c = k % sb.cols, r = Math.floor(k / sb.cols);
  Object.assign(el.style, { backgroundImage: `url("${sb.sheets[Math.floor(idx / per)]}")`, backgroundSize: `${sb.cols * w}px ${sb.rows * hh}px`, backgroundPosition: `${-c * w}px ${-r * hh}px` });
  return el;
}
function renderFilm() {
  TL.film.innerHTML = ''; const sb = S.vod && S.vod.storyboard;
  TL.film.classList.toggle('none', !sb); if (!sb) return;
  const W = TL.track.clientWidth, H = TL.track.clientHeight; const tw = H * sb.w / sb.h; const n = Math.ceil(W / tw);
  for (let i = 0; i < n; i++) { const t = S.tl.v0 + (i + .5) * tw / W * (S.tl.v1 - S.tl.v0); TL.film.append(sprite(h('div', { class: 'tile', style: `width:${tw}px` }), t, tw, H)); }
}
function renderTicks() {
  const { v0, v1 } = S.tl; const W = TL.track.clientWidth || 800;
  const step = [1, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200].find(s => W * s / (v1 - v0) >= 84) || 7200;
  TL.ticks.innerHTML = '';
  for (let t = Math.ceil(v0 / step) * step; t <= v1 + .01; t += step) { const x = xOf(t); if (x > 2 && x < 98) TL.ticks.append(h('span', { style: `left:${x}%` }, fmt(t))); }
}
function setView(a, b) {
  const span = b - a; if (a < 0) { a = 0; b = span; } if (b > S.tl.dur) { b = S.tl.dur; a = Math.max(0, b - span); }
  S.tl.v0 = a; S.tl.v1 = b; renderFilm(); renderTicks(); renderTL();
}
function zoomSel() { const { lo, hi } = S.tl; const span = Math.max(hi - lo, 20); const pad = Math.max(span * .25, 10); setView(lo - pad, hi + pad); }
function setHandle(i, t) { t = clamp(t, 0, S.tl.dur); if (i === 0) S.tl.lo = Math.min(t, S.tl.hi); else S.tl.hi = Math.max(t, S.tl.lo); }
function follow(t) { const { v0, v1 } = S.tl; if (t < v0 || t > v1) { const w = v1 - v0; setView(t - w / 2, t + w / 2); } }

function initTimeline() {
  const drag = { mode: null, grab: 0 };
  TL.track.addEventListener('pointerdown', e => {
    if (!S.vod || e.button !== 0) return;
    TL.el.focus(); const t = tAt(e.clientX); const hd = e.target.closest('.tl-handle');
    if (hd) drag.mode = +hd.dataset.h;
    else if (e.target === TL.sel) { drag.mode = 2; drag.grab = t - S.tl.lo; }
    else { drag.mode = Math.abs(t - S.tl.lo) < Math.abs(t - S.tl.hi) ? 0 : 1; setHandle(drag.mode, t); }
    if (drag.mode < 2) { S.tl.active = drag.mode; TL.hs[drag.mode].classList.add('drag'); }
    TL.track.setPointerCapture(e.pointerId); schedule();
  });
  TL.track.addEventListener('pointermove', e => {
    if (!S.vod) return;
    const t = tAt(e.clientX);
    if (drag.mode === 2) { const span = S.tl.hi - S.tl.lo; S.tl.lo = clamp(t - drag.grab, 0, S.tl.dur - span); S.tl.hi = S.tl.lo + span; }
    else if (drag.mode != null) setHandle(drag.mode, t);
    hoverAt(e.clientX, drag.mode == null ? t : drag.mode === 1 ? S.tl.hi : S.tl.lo);
    if (drag.mode != null) schedule();
  });
  const end = () => { if (drag.mode == null) return; drag.mode = null; TL.hs.forEach(x => x.classList.remove('drag')); commitRange(); };
  TL.track.addEventListener('pointerup', end); TL.track.addEventListener('pointercancel', end);
  TL.track.addEventListener('pointerleave', () => { if (drag.mode == null) { TL.pop.classList.remove('on'); TL.cursor.classList.remove('on'); } });
  TL.track.addEventListener('wheel', e => {
    if (!S.vod) return; e.preventDefault();
    const { v0, v1, dur } = S.tl;
    if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) { const d = (e.deltaX || e.deltaY) / 800 * (v1 - v0); setView(v0 + d, v1 + d); return; }
    const t = tAt(e.clientX); const span = clamp((v1 - v0) * (e.deltaY > 0 ? 1.25 : .8), 20, dur); const r = (t - v0) / (v1 - v0);
    setView(t - r * span, t - r * span + span); hoverAt(e.clientX, t);
  }, { passive: false });
  TL.track.addEventListener('dblclick', () => setView(0, S.tl.dur));
  TL.el.addEventListener('keydown', e => {
    if (!S.vod) return;
    if (e.key === 'ArrowUp' || e.key === 'ArrowDown') { e.preventDefault(); S.tl.active ^= 1; renderTL(); return; }
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    e.preventDefault(); const step = e.ctrlKey ? 60 : e.shiftKey ? 10 : 1;
    const i = S.tl.active; const t = (i ? S.tl.hi : S.tl.lo) + (e.key === 'ArrowLeft' ? -step : step);
    setHandle(i, t); follow(t); renderTL(); commitRange();
  });
  TL.el.addEventListener('focus', renderTL); TL.el.addEventListener('blur', renderTL);
  new ResizeObserver(() => { if (S.vod) { renderFilm(); renderTicks(); } }).observe(TL.track);
  $$('.nudge').forEach(b => b.onclick = () => { const i = +b.dataset.h; const t = (i ? S.tl.hi : S.tl.lo) + +b.dataset.d; S.tl.active = i; setHandle(i, t); follow(t); renderTL(); commitRange(); });
  [['#startIn', 0], ['#endIn', 1]].forEach(([s, i]) => {
    const el = $(s);
    const apply = () => {
      const v = parseTs(el.value); if (v == null) { el.classList.remove('bad'); void el.offsetWidth; el.classList.add('bad'); toast('Times look like 1:02:30, 62:30, or 1h2m30s', 'warn', 2500); return; }
      el.classList.remove('bad');
      if (i === 0) { S.tl.lo = clamp(v, 0, S.tl.dur); if (S.tl.hi < S.tl.lo) S.tl.hi = S.tl.lo; } else { S.tl.hi = clamp(v, 0, S.tl.dur); if (S.tl.lo > S.tl.hi) S.tl.lo = S.tl.hi; }
      if (S.tl.lo < S.tl.v0 || S.tl.hi > S.tl.v1) zoomSel(); renderTL(); commitRange();
    };
    el.addEventListener('keydown', e => { if (e.key === 'Enter') { apply(); el.blur(); } });
    el.addEventListener('change', apply);
  });
  $('#zoomSel').onclick = zoomSel; $('#zoomAll').onclick = () => setView(0, S.tl.dur);
}
function hoverAt(clientX, t) {
  const r = TL.track.getBoundingClientRect(); const x = clamp(clientX - r.left, 0, r.width);
  TL.cursor.style.left = x + 'px'; TL.cursor.classList.add('on');
  TL.pop.style.left = clamp(x, 90, r.width - 90) + 'px'; TL.pop.classList.add('on');
  const img = $('.img', TL.pop); img.classList.toggle('none', !S.vod.storyboard); sprite(img, t, 176, 99);
  $('b', TL.pop).textContent = fmt(t);
}
const loadFrames = debounce(() => { loadFrame(0); loadFrame(1); }, 250);
function commitRange(now) { now ? (loadFrame(0), loadFrame(1)) : loadFrames(); }
const frameReq = [0, 0];
async function loadFrame(i) {
  if (!S.vod) return;
  const t = Math.round(i ? S.tl.hi : S.tl.lo); const box = i ? $('#frameEnd') : $('#frameStart'); const img = $('img', box);
  const key = S.vod.id + ':' + t; const req = ++frameReq[i];
  const show = u => { img.onload = () => { if (req === frameReq[i]) { img.classList.add('on'); box.classList.remove('loading'); } }; img.src = u; };
  if (S.frameCache.has(key)) return show(S.frameCache.get(key));
  img.classList.remove('on'); box.classList.add('loading');
  sprite(box, t, box.clientWidth, box.clientHeight);   // instant low-res placeholder
  try { const u = await api('vod.frame', { id: S.vod.id, t, w: 640 }); S.frameCache.set(key, u); if (req === frameReq[i]) show(u); }
  catch { if (req === frameReq[i]) box.classList.remove('loading'); }
}

/* ---------- sync ---------- */
async function myVods() {
  const ch = (S.settings.my_channel || '').trim();
  if (!ch) { toast('Add your Twitch channel in Settings first.', 'warn'); showPage('settings'); setTimeout(() => $('#setChannel').focus(), 350); return; }
  const btn = $('#myVodsBtn'); btn.classList.add('busy');
  try {
    const r = await api('source.load', { text: ch }); const list = $('#myVodList'); list.innerHTML = '';
    (r.vods || []).forEach(v => list.append(h('button', { class: 'pop-item', onclick: () => { $('#myVod').value = v.url; list.hidden = true; $('#myStart').focus(); } },
      h('img', { src: v.thumbnail || '' }), h('div', {}, h('b', {}, v.title), h('span', {}, `${v.start_ts ? ago(v.start_ts) : ''} · ${durTxt(v.duration)}`)))));
    list.hidden = false;
  } catch (e) { toast(e.message, 'bad', 7000); } finally { btn.classList.remove('busy'); }
}
async function runSync() {
  const mine = $('#myVod').value.trim(), a = parseTs($('#myStart').value), b = parseTs($('#myEnd').value);
  const typed = $('#syncTheirs input').value.trim();
  if (typed) { syncTheirs.set([...syncTheirs.get(), ...typed.split(',').map(s => s.trim()).filter(Boolean)]); $('#syncTheirs input').value = ''; }
  const theirs = syncTheirs.get();
  if (!mine) return $('#myVod').focus();
  if (a == null) { $('#myStart').classList.add('bad'); return $('#myStart').focus(); }
  if (b == null || b <= a) { $('#myEnd').classList.add('bad'); return $('#myEnd').focus(); }
  if (!theirs.length) return $('#syncTheirs input').focus();
  $$('#myStart,#myEnd').forEach(x => x.classList.remove('bad'));
  const btn = $('#syncGo'), res = $('#syncResult'); btn.classList.add('busy'); res.hidden = true; $('#syncList').hidden = true;
  try {
    const r = await api('sync.match', { mine, my_start: a, my_end: b, theirs });
    const ok = r.results.filter(x => x.ok);
    res.hidden = false;
    if (ok.length) {
      res.className = 'banner';
      res.innerHTML = `<b>Found ${ok.length} of ${r.results.length}.</b> Your <b>${fmt(a)} – ${fmt(b)}</b> mapped onto their VODs, padded ±${SYNC_PAD}s for stream delay. Fine-tune any of them, or translate them all.`;
    } else { res.className = 'banner err'; res.textContent = 'No matches. ' + r.results.map(x => `${x.input}: ${x.error}`).join('\n'); }
    renderSyncList(r.results);
    if (ok.length) loadSyncResult(ok[0]);
  } catch (e) { res.className = 'banner err'; res.hidden = false; res.textContent = e.message; }
  finally { btn.classList.remove('busy'); }
}
const SYNC_PAD = 15;
const syncRange = x => [Math.max(0, x.start - SYNC_PAD), Math.min(x.theirs.duration, x.end + SYNC_PAD)];
function loadSyncResult(x) {
  S.sync = { offset: x.offset, theirId: x.theirs.id };
  setVod(x.theirs, syncRange(x));
  $$('.sync-row').forEach(r => r.classList.toggle('view', r.dataset.id === x.theirs.id));
}
function renderSyncList(results) {
  const list = $('#syncList'); list.innerHTML = ''; list.hidden = false;
  results.forEach(x => {
    if (!x.ok) { list.append(h('div', { class: 'sync-row err' }, h('span', {}, '✕'), h('div', {}, h('b', {}, x.input), h('span', {}, x.error)))); return; }
    const [lo, hi] = syncRange(x);
    const rel = x.offset > 0 ? `went live ${spanTxt(x.offset)} before you` : x.offset < 0 ? `went live ${spanTxt(x.offset)} after you` : 'went live with you';
    const row = h('div', { class: 'sync-row', 'data-id': x.theirs.id },
      h('input', { type: 'checkbox', checked: true, title: 'Include in "Translate all"', onchange: updateSyncFoot }),
      h('img', { src: x.theirs.thumbnail || '', alt: '' }),
      h('div', {}, h('b', {}, x.theirs.channel), h('span', {}, `their ${fmt(lo)} – ${fmt(hi)} · ${rel}${x.clipped ? ' · trimmed to their VOD' : ''}`)),
      h('button', { class: 'btn ghost sm', onclick: () => loadSyncResult(x) }, 'Fine-tune'));
    row._res = x; list.append(row);
  });
  if (results.filter(x => x.ok).length > 1)
    list.append(h('div', { class: 'sync-foot' }, h('span', { class: 'muted small', style: 'margin:0' }, 'Each one is downloaded, translated and saved on its own, one after another.'),
      h('button', { class: 'btn primary', id: 'syncAll', onclick: translateAllSync }, 'Translate all')));
  updateSyncFoot();
}
function updateSyncFoot() { const b = $('#syncAll'); if (!b) return; const n = $$('.sync-row input:checked').length; b.textContent = `Translate all ${n}`; b.disabled = !n; }
async function translateAllSync() {
  const jobs = $$('.sync-row').filter(r => r._res && $('input', r).checked).map(r => {
    const x = r._res; let [lo, hi] = syncRange(x);
    if (S.vod && S.vod.id === x.theirs.id) { lo = S.tl.lo; hi = S.tl.hi; }   // keep any fine-tuning
    return jobParams(x.theirs, lo, hi);
  });
  if (jobs.length) await queueJobs(jobs);
}

/* ---------- snap to game ---------- */
async function runSnap() {
  if (!S.vod) return;
  const b = $('#snapBtn'), label = $('span', b);
  if (b.classList.contains('running')) { api('snap.cancel'); return; }
  b.classList.add('running'); label.textContent = 'Reading the clock…'; b.title = 'Click to stop';
  try {
    const r = await api('snap.run', { id: S.vod.id, lo: S.tl.lo, hi: S.tl.hi });
    S.tl.lo = r.start; S.tl.hi = r.end;
    zoomSel(); renderTL(); commitRange(true);
    toast(`Game found. The game clock hit 0:00 at ${fmt(r.game_start)} and the game ends around ${fmt(r.end)} (${r.readings} clock readings).`, 'good', 7000);
  } catch (e) { if (e.message !== 'Cancelled') toast(e.message, 'bad', 9000); }
  finally { b.classList.remove('running'); label.textContent = 'Snap to game'; b.title = 'Read the in-game clock and set the section to exactly the game'; }
}
on('snap.progress', m => { const b = $('#snapBtn'); if (b.classList.contains('running')) $('span', b).textContent = `${m.text.replace('…', '')} · ${Math.round(m.f * 100)}%`; });

/* ---------- job queue ---------- */
const STEP_NAMES = { 'Reading VOD info': 'Info', 'Downloading section': 'Download', 'Downloading audio': 'Audio', 'Transcribing speech': 'Transcribe', 'Loading Whisper': 'Whisper', 'Isolating voices': 'Voices', 'Detecting language': 'Language', 'Translating to English': 'Translate', 'Transcribing original language': 'Original', 'Burning subtitles into video': 'Subtitles' };
S.jobs = {};
S.viewJob = null;
const J = id => (S.jobs[id] = S.jobs[id] || { steps: [], lines: [], start: 0, f: -1, stage: 0 });
const finished = id => { const j = S.jobs[id]; return !j || j.result || j.error || j.cancelled; };
function jobParams(vod, lo, hi) {
  return { url: vod.url, vod_id: vod.id, start: lo, end: hi, out_dir: S.settings.out_dir, quality: qualSeg.get(), model: modelSeg.get(),
    language: $('#langSel').value, orig: $('#origTog').checked, burn: $('#burnTog').checked, position: $('#posPick .on').dataset.v,
    size: sizeSeg.get(), champions: champsClip.get(), use_glossary: $('#glossClip').checked, isolate: $('#isoClip').checked,
    method: methodSeg.get(), auto_details: $('#autoClip').checked, speakers: $('#spkClip').checked,
    label: `${vod.channel} · ${fmt(lo)}–${fmt(hi)}` };
}
// Twitch, YouTube and Kick VOD links (for paste/clipboard detection) and "open at this moment" links
const VOD_RX = /https?:\/\/(?:www\.|m\.)?(?:twitch\.tv\/videos\/\d+|youtube\.com\/(?:watch\?\S*v=|live\/)[\w-]{11}|youtu\.be\/[\w-]{11}|kick\.com\/[\w-]+\/videos?\/[0-9a-f-]{36})\S*/i;
function vodAt(url, id, t) {
  url = url || ''; t = Math.max(0, Math.floor(t || 0));
  if (/twitch\.tv/.test(url) && id) return `https://www.twitch.tv/videos/${id}?t=${hmsTag(t)}`;
  if (/youtu\.?be/.test(url) && id) return `https://www.youtube.com/watch?v=${id}&t=${t}s`;
  return url;
}
function stepsFor(p) {
  const local = isLocal(p.url);
  const s = [local ? 'Reading the video file' : 'Reading VOD info', local ? 'Cutting out the audio' : 'Downloading audio', 'Loading Whisper'];
  if (p.isolate) s.push('Isolating voices');
  s.push('Detecting language');
  if (p.method === 'two-step') s.push('Transcribing speech', 'Translating to English');
  else { s.push('Translating to English'); if (p.orig) s.push('Transcribing original language'); }
  if (p.speakers) s.push('Telling speakers apart');
  if (p.burn) s.push(local ? 'Cutting out the video' : 'Downloading video', 'Burning subtitles into video');
  return s;
}
async function queueJobs(list) {
  try {
    const ids = await api('queue.add', { jobs: list.map(p => ({ ...p })) });
    ids.forEach((id, i) => Object.assign(J(id), { p: list[i], steps: list[i].kind === 'burn' ? ['Burning subtitles into video'] : stepsFor(list[i]),
      url: list[i].url, vodId: list[i].vod_id, start: list[i].start || 0, label: list[i].label }));
    if (!S.viewJob || finished(S.viewJob)) viewJob(ids[0]);
    showRun();
    if (list.length > 1) toast(`${list.length} translations queued`, 'good', 2500);
    return ids;
  } catch (e) { toast(e.message, 'bad'); }
}
async function startClip() {
  if (!S.vod) return;
  const p = jobParams(S.vod, S.tl.lo, S.tl.hi);
  if (p.end - p.start > 7200 && !confirm(`That section is ${fmt(p.end - p.start)} long. Translate all of it?`)) return;
  await queueJobs([p]);
}
function showRun() {
  const card = $('#runCard');
  if (card.hidden) { card.hidden = false; card.classList.remove('reveal'); void card.offsetWidth; card.classList.add('reveal'); }
  setTimeout(() => card.scrollIntoView({ behavior: 'smooth', block: 'start' }), 100);
}
function setGo(busy) { const g = $('#goBtn'); g.disabled = !S.vod; $('span', g).textContent = busy ? 'Add to queue' : 'Translate this section'; }
function setBar(f) {
  const bar = $('#barFill').parentElement; bar.classList.toggle('indet', f < 0); bar.classList.toggle('running', f >= 0 && f < 1);
  if (f >= 0) $('#barFill').style.width = (f * 100).toFixed(1) + '%';
}
on('queue.update', m => renderQueue(m.jobs));
function renderQueue(jobs) {
  S.queue = jobs;
  const busy = jobs.some(j => j.status === 'running' || j.status === 'pending');
  setGo(busy);
  $('#cancelBtn').hidden = !jobs.some(j => j.status === 'running');
  $('#queueClear').hidden = jobs.length < 2 || !jobs.some(j => ['done', 'error', 'cancelled'].includes(j.status));
  jobs.forEach(j => { const x = J(j.id); x.label = x.label || j.label; if (j.status === 'cancelled') x.cancelled = true; if (j.status === 'error') x.error = x.error || j.error; });
  const list = $('#queueList'); list.hidden = jobs.length < 2; list.innerHTML = '';
  jobs.forEach((j, i) => {
    const x = J(j.id);
    list.append(h('div', { class: `qrow ${j.status}${S.viewJob === j.id ? ' view' : ''}`, 'data-id': j.id, onclick: () => viewJob(j.id) },
      h('span', { class: 'st' }, { done: '✓', error: '!', cancelled: '–' }[j.status] || String(i + 1)),
      h('span', {}, j.label + (j.status === 'pending' ? ' · waiting' : '')),
      h('div', { class: 'mini' }, h('i', { style: `width:${j.status === 'done' ? 100 : Math.round(overall(x) * 100)}%` })),
      ['pending', 'running'].includes(j.status) ? h('button', { class: 'x', title: 'Cancel this one', onclick: e => { e.stopPropagation(); api('queue.cancel', { id: j.id }); } }, '✕') : h('span')));
  });
  if (jobs.length && $('#runCard').hidden) $('#runCard').hidden = false;
}
const overall = x => x.result ? 1 : !x.stage ? 0 : clamp((x.stage - 1 + Math.max(0, x.f)) / Math.max(x.n || x.steps.length || 1, 1), 0, 1);
function updateMini(id) { const row = $(`.qrow[data-id="${id}"] .mini i`); if (row) row.style.width = Math.round(overall(J(id)) * 100) + '%'; }
function viewJob(id) {
  S.viewJob = id; const x = J(id);
  $$('.qrow').forEach(r => r.classList.toggle('view', r.dataset.id === id));
  paintSteps(x); setBar(x.result ? 1 : x.stage ? x.f : 0);
  $('#runTitle').textContent = x.result ? doneTitle(x.result) : x.error ? 'Something went wrong' : x.cancelled ? 'Cancelled' : x.stage ? 'Translating…' : 'Waiting in the queue…';
  $('#runStatus').textContent = x.result ? `Done in ${spanTxt(x.result.seconds)}` : x.error ? x.error.split('\n')[0] : x.label ? (x.stage ? `${x.stageLabel || ''}…` : x.label) : '';
  $('#runEta').textContent = '';
  const feed = $('#clipFeed'); feed.innerHTML = ''; x.lines.forEach(l => feed.append(lineEl(x, l)));
  showResults(x);
}
function paintSteps(x) {
  const st = $('#stepper'); const steps = x.steps.length ? x.steps : [x.stageLabel || 'Working'];
  if (st.childElementCount !== steps.length || st.dataset.job !== S.viewJob) {
    st.innerHTML = ''; st.dataset.job = S.viewJob;
    steps.forEach((s, i) => st.append(h('div', { class: 'step' }, h('i', {}, i + 1), STEP_NAMES[s] || s)));
  }
  $$('.step', st).forEach((s, i) => {
    const done = x.result || i < x.stage - 1; s.classList.toggle('done', !!done); s.classList.toggle('active', !x.result && i === x.stage - 1);
    $('i', s).innerHTML = done ? '<span class="ic ic-check" style="width:11px;height:11px"></span>' : String(i + 1);
  });
}
function lineEl(x, l) {
  if (l.log) return h('div', { class: 'line log' }, l.text);
  const at = (x.start || 0) + l.start;
  return h('div', { class: 'line' + (l.kind === 'orig' ? ' orig' : '') },
    h('span', { class: 'ts', title: 'Open the VOD at this moment', onclick: () => api('os.url', { url: vodAt(x.url, x.vodId, at) }) }, fmt(at)),
    h('div', { class: 'tx' }, l.text));
}
function addLine(id, l) {
  const x = J(id); x.lines.push(l); if (x.lines.length > 4000) x.lines.shift();
  if (S.viewJob !== id) return;
  const feed = $('#clipFeed'); const near = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 60;
  feed.append(lineEl(x, l)); if (near) feed.scrollTop = feed.scrollHeight;
}
const doneTitle = r => r.kind === 'burn' ? 'Edited subtitles burned in' : `${r.language_name} → English · ${r.segments} lines`;
function showResults(x) {
  const r = $('#results'); const res = x.result;
  r.hidden = !res || res.kind === 'burn' && !res.subbed;
  if (!res) return;
  S.lastResult = res.kind === 'burn' ? { ...(S.lastResult || {}), ...res } : res;
  $('.res[data-open=subbed] b').textContent = res.subbed ? 'Play video' : 'Play clip';
  $('.res[data-open=subbed] span').textContent = res.subbed ? 'subtitles burned in' : 'no subtitles burned';
  $('.res[data-open=edit]').hidden = res.kind === 'burn';
}
on('clip.stage', m => {
  const x = J(m.job); x.stage = m.i; x.n = m.n; x.stageLabel = m.label; x.stageT = Date.now(); x.f = -1;
  if (m.i === 1 && (!S.viewJob || finished(S.viewJob) || S.viewJob === m.job)) { viewJob(m.job); showRun(); }
  if (S.viewJob === m.job) { paintSteps(x); setBar(-1); $('#runTitle').textContent = 'Translating…'; $('#runStatus').textContent = `${m.label}…`; $('#runEta').textContent = ''; $('#results').hidden = true; }
  updateMini(m.job);
});
on('clip.progress', m => {
  const x = J(m.job); x.f = m.f; updateMini(m.job);
  if (S.viewJob !== m.job) return;
  setBar(m.f);
  if (m.f >= 0) {
    $('#runStatus').textContent = `${x.stageLabel}… ${Math.round(m.f * 100)}%`;
    const el = (Date.now() - x.stageT) / 1000;
    $('#runEta').textContent = m.f > .06 && m.f < 1 && el > 3 ? `about ${spanTxt(el * (1 - m.f) / m.f)} left` : '';
  }
});
on('clip.segment', m => addLine(m.job, { kind: m.kind, start: m.start, text: m.text }));
on('clip.log', m => addLine(m.job, { log: true, text: m.text }));
on('clip.done', m => {
  const x = J(m.job); x.result = m; updateMini(m.job);
  if (m.kind === 'burn') { toast('The edited video is ready.', 'good'); onBurnDone(m); }
  else toast(`Done: ${m.channel || ''} · ${m.segments} lines translated from ${m.language_name}.`, 'good');
  if (S.viewJob === m.job) viewJob(m.job);
});
on('clip.error', m => {
  const x = J(m.job); x.error = m.message; addLine(m.job, { log: true, text: m.message });
  if (m.report) actionToast(m.message.split('\n')[0], 'bad', [['Report', () => reportProblem(`A translation failed: ${m.message.split('\n')[0]}`)]]);
  else toast(m.message.split('\n').slice(0, 6).join('\n'), 'bad', 10000);
  if (S.viewJob === m.job) viewJob(m.job);
  onBurnDone(null, m);
});
on('clip.cancelled', m => { const x = J(m.job); x.cancelled = true; if (S.viewJob === m.job) viewJob(m.job); onBurnDone(null, m); });

async function browseOut() {
  const d = await api('os.browse', { current: S.settings.out_dir }); if (!d) return;
  setSetting('out_dir', d); $('#outDir').value = d; $('#setOut').value = d;
}

/* ================================================================ LIVE */
const METER_N = 18; let meterBars = [], meterLv = 0;
function initLive() {
  const st = S.settings;
  $('#liveLang').value = st.live_language || '';
  $('#liveLang').onchange = e => { setSetting('live_language', e.target.value); if (S.live.on) api('live.update', { language: e.target.value }); };
  liveModel = seg($('#liveModelSeg'), v => setSetting('live_model', v)); liveModel.set(st.live_model);
  liveMethod = seg($('#liveMethodSeg'), v => { setSetting('live_method', v); if (S.live.on) api('live.update', { method: v }); }); liveMethod.set(st.live_method || 'two-step');
  $('#liveOrig').checked = st.live_orig; $('#liveOrig').onchange = e => { setSetting('live_orig', e.target.checked); if (S.live.on) api('live.update', { orig: e.target.checked }); };
  $('#liveGloss').onchange = e => S.live.on && api('live.update', { use_glossary: e.target.checked });
  $('#liveIso').checked = st.live_isolate;
  $('#liveIso').onchange = e => { setSetting('live_isolate', e.target.checked); if (S.live.on) api('live.update', { isolate: e.target.checked }); };
  $('#liveChat').checked = st.live_chat;
  $('#liveChat').onchange = e => { setSetting('live_chat', e.target.checked); if (S.live.on) api('chat.toggle', { on: e.target.checked }); };
  $('#chatClear').onclick = () => { $('#chatFeed').innerHTML = ''; };
  const cf = $('#chatFeed');
  cf.addEventListener('scroll', () => { if (cf.scrollHeight - cf.scrollTop - cf.clientHeight < 40) $('#chatJump').hidden = true; });
  $('#chatJump').onclick = () => { cf.scrollTo({ top: cf.scrollHeight, behavior: 'smooth' }); $('#chatJump').hidden = true; };
  champsLive = chipInput($('#champsLive'), { placeholder: 'Type a champion…', onChange: c => S.live.on && api('live.update', { champions: c }) });
  $('#liveBtn').onclick = toggleLive;
  $('#liveInput').addEventListener('keydown', e => { if (e.key === 'Enter' && !S.live.on) toggleLive(); });
  $('#ovFont').value = st.overlay_font; $('#ovBg').value = Math.round(st.overlay_bg * 100);
  const ovLabels = () => { $('#ovFontV').textContent = $('#ovFont').value + 'px'; $('#ovBgV').textContent = $('#ovBg').value + '%'; }; ovLabels();
  const ovStyle = () => { ovLabels(); const font = +$('#ovFont').value, bg = $('#ovBg').value / 100; S.settings.overlay_font = font; S.settings.overlay_bg = bg; saveSettings(); api('overlay.style', { font, bg }); };
  $('#ovFont').oninput = ovStyle; $('#ovBg').oninput = ovStyle;
  $('#ovTog').onchange = async e => { const shown = await api('overlay.show', { show: e.target.checked }); if (shown) api('overlay.style', { font: +$('#ovFont').value, bg: $('#ovBg').value / 100, orig: $('#liveOrig').checked }); };
  $('#obsBtn').onclick = async () => { const url = await api('obs.url'); const box = $('#obsUrl'); $('code', box).textContent = url; box.hidden = false; };
  $('#obsCopy').onclick = () => { api('os.copy', { text: $('#obsUrl code').textContent }); toast('OBS link copied', 'good', 2000); };
  $('#liveOpenTx').onclick = () => S.live.transcript && api('os.open', { path: S.live.transcript });
  $('#liveClear').onclick = () => { $('#liveFeed').innerHTML = ''; };
  const feed = $('#liveFeed');
  feed.addEventListener('scroll', () => { if (feed.scrollHeight - feed.scrollTop - feed.clientHeight < 40) $('#jumpLatest').hidden = true; });
  $('#jumpLatest').onclick = () => { feed.scrollTo({ top: feed.scrollHeight, behavior: 'smooth' }); $('#jumpLatest').hidden = true; };
  const meter = $('#meter'); for (let i = 0; i < METER_N; i++) meter.append(h('i')); meterBars = $$('i', meter);
  const shape = [...Array(METER_N)].map((_, i) => .45 + .55 * Math.sin(Math.PI * (i + .5) / METER_N));
  // animate only while there's sound to show (keeps the GPU idle otherwise)
  meterTick = function tick() {
    meterLv *= .9;
    meterBars.forEach((b, i) => { const v = meterLv * shape[i] * (.75 + Math.random() * .5); b.style.height = clamp(6 + v * 94, 6, 100) + '%'; });
    if (meterLv > 0.004) requestAnimationFrame(tick); else { meterRunning = false; meterBars.forEach(b => b.style.height = '6%'); }
  };
}
let meterTick = null, meterRunning = false;
async function toggleLive() {
  if (S.live.on || S.live.starting) { api('live.stop'); return; }
  const source = $('#liveInput').value.trim(); if (!source) { $('#liveInput').focus(); return; }
  S.live.starting = true; liveButton(true);
  $('#liveState').textContent = 'Connecting…';
  try {
    await api('live.start', { source, model: liveModel.get(), language: $('#liveLang').value, orig: $('#liveOrig').checked, use_glossary: $('#liveGloss').checked,
      champions: champsLive.get(), isolate: $('#liveIso').checked, method: liveMethod.get(), chat: $('#liveChat').checked });
    if ($('#liveChat').checked) { $('#chatFeed').innerHTML = ''; $('#chatStatus').textContent = 'Connecting to chat…'; }
  } catch (e) { toast(e.message, 'bad'); S.live.starting = false; liveButton(false); }
}
function liveButton(on) { const b = $('#liveBtn'); b.classList.toggle('on', on); $('b', b).textContent = on ? 'Stop' : 'Go live'; $('#liveInput').disabled = on; $$('#liveModelSeg button').forEach(x => x.disabled = on); }
on('live.status', m => {
  const st = $('#liveState');
  if (m.channel) $('#liveChannel').textContent = m.channel;
  if (m.title) $('#liveTitle').textContent = m.title;
  if (m.transcript) { S.live.transcript = m.transcript; $('#liveOpenTx').disabled = false; }
  if (m.state === 'live') {
    if (!S.live.on) {
      S.live.on = true; S.live.starting = false; S.live.started = Date.now(); S.live.lines = 0;
      clearInterval(S.live.timer); S.live.timer = setInterval(() => $('#stUp').textContent = fmt((Date.now() - S.live.started) / 1000), 1000);
      $('#liveFeed').innerHTML = ''; toast(`Live: translating ${m.channel || 'the stream'}`, 'good', 2500);
    }
    document.body.classList.add('live-on'); $('.live-pill').hidden = false; liveButton(true);
    st.textContent = '● Live · listening'; st.style.color = 'var(--live)';
    if (m.device === 'CPU') toast('Whisper is running on the CPU. Live captions will lag a lot.', 'warn', 8000);
    $('#nowText').className = 'now-text idle'; $('#nowText').textContent = 'Listening for speech…';
  } else if (m.state === 'stopped') {
    const was = S.live.on; S.live.on = false; S.live.starting = false; clearInterval(S.live.timer);
    document.body.classList.remove('live-on'); $('.live-pill').hidden = true; liveButton(false);
    st.textContent = 'Stopped'; st.style.color = '';
    if (was) toast(`Stopped. ${m.lines || 0} lines saved to the transcript.`, '', 4000);
  } else { st.textContent = m.message || m.state; st.style.color = m.state === 'reconnecting' ? 'var(--warn)' : ''; }
  if (m.thumbnail && !$('#liveThumb img').src.startsWith('data:')) setThumb(m.thumbnail);
});
function setThumb(src) { const img = $('#liveThumb img'); img.onload = () => { img.hidden = false; $('#liveThumb .thumb-empty').hidden = true; }; img.src = src; }
on('live.frame', m => setThumb(m.data));
on('live.level', m => {
  meterLv = Math.max(meterLv, m.level);
  if (!meterRunning && meterTick && !document.hidden) { meterRunning = true; requestAnimationFrame(meterTick); }
});
on('live.lang', m => { $('#stLang').textContent = m.name + (m.locked ? '' : '?'); $('#stLang').title = m.locked ? 'Detected and locked in' : 'Still detecting'; });
on('live.partial', m => {
  if (!m.text) return; const t = $('#nowText'); t.className = 'now-text partial'; t.textContent = m.text; $('#nowOrig').textContent = '';
});
on('live.caption', m => {
  const t = $('#nowText'); t.className = 'now-text'; void t.offsetWidth; t.classList.add('flash'); t.textContent = m.text;
  $('#nowOrig').textContent = $('#liveOrig').checked ? (m.orig || '') : '';
  S.live.lines = m.id; $('#stLines').textContent = m.id; $('#stDelay').textContent = (m.delay + m.took).toFixed(1) + 's';
  if (!$('#stLang').textContent || $('#stLang').textContent === '—') $('#stLang').textContent = LANGS[m.lang] || m.lang;
  const feed = $('#liveFeed'); const near = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 60;
  const empty = $('.empty', feed); if (empty) empty.remove();
  feed.append(h('div', { class: 'line' }, h('span', { class: 'ts' }, m.clock), h('div', { class: 'tx' }, m.text, m.orig && $('#liveOrig').checked ? h('small', {}, m.orig) : null)));
  if (near) feed.scrollTop = feed.scrollHeight; else $('#jumpLatest').hidden = false;
});
on('live.error', m => {
  toast(m.message, 'bad', 8000);
  if (m.fatal !== false && !S.live.on) { S.live.starting = false; liveButton(false); $('#liveState').textContent = 'Not live'; }
});

/* ================================================================ GLOSSARY */
function applyGloss(g) {
  S.gloss = g; S.champs = g.champions || [];
  if (!$('#champWall')) return;
  $('#champCount').textContent = `${S.champs.length} champions` + (g.version ? ` · patch ${g.version}` : '');
  renderWall();
  $('#termCloud').innerHTML = ''; (g.core_terms || []).forEach(t => $('#termCloud').append(h('span', {}, t)));
  $('#termCloud').classList.toggle('off', !g.use_core_terms);
  $('#glossOn').checked = g.enabled; $('#coreOn').checked = g.use_core_terms;
}
function renderWall() {
  const q = ($('#champFilter').value || '').toLowerCase(); const wall = $('#champWall'); wall.innerHTML = '';
  S.champs.filter(c => c.name.toLowerCase().includes(q)).forEach((c, i) => wall.append(h('div', { class: 'c', title: c.name, style: `animation-delay:${Math.min(i, 60) * 8}ms` }, champIcon(c))));
}
const saveGloss = debounce(async () => {
  const fixes = {}; $$('#fixTable .fix-row').forEach(r => { const [a, b] = $$('input', r).map(x => x.value.trim()); if (a && b) fixes[a] = b; });
  const slang = {}; $$('#slangTable .fix-row').forEach(r => { const [a, b] = $$('input', r).map(x => x.value.trim()); if (a && b) slang[a] = b; });
  try { const g = await api('glossary.save', { data: { enabled: $('#glossOn').checked, use_core_terms: $('#coreOn').checked, custom_words: customWords.get(), fixes, slang, use_slang: $('#slangOn').checked } }); S.gloss = g; tryIt(); }
  catch (e) { toast(e.message, 'bad'); }
}, 450);
function fixRow(a = '', b = '', ph = ['wrong', 'right']) {
  const row = h('div', { class: 'fix-row' }, h('input', { value: a, placeholder: ph[0], oninput: saveGloss }), h('span', { class: 'arrow' }, '→'),
    h('input', { value: b, placeholder: ph[1], oninput: saveGloss }), h('button', { title: 'Remove', onclick: () => { row.remove(); saveGloss(); } }, '×'));
  return row;
}
const SLANG_PH = ['phrase as they say it (any language)', 'what it means, in English'];
function initSlang() {
  const g = S.gloss;
  $('#slangOn').checked = g.use_slang !== false; $('#slangOn').onchange = saveGloss;
  $('#slangCount').textContent = g.builtin_slang ? `${g.builtin_slang} common phrases are built in (Spanish, Portuguese, French, German); add your own below.` : '';
  Object.entries(g.slang || {}).forEach(([a, b]) => $('#slangTable').append(fixRow(a, b, SLANG_PH)));
  $('#addSlang').onclick = () => { const r = fixRow('', '', SLANG_PH); $('#slangTable').append(r); $('input', r).focus(); };
  $('#slangTest').onclick = async () => {
    const b = $('#slangTest'), out = $('#slangOut'); b.classList.add('busy'); out.textContent = 'Translating… (the first time loads the translator)';
    try {
      const r = await api('glossary.slang_test', { text: $('#slangIn').value, lang: $('#slangLang').value });
      out.innerHTML = '';
      out.append(h('div', { class: 'muted small' }, 'Without slang help: ' + r.plain),
        h('div', {}, h('b', {}, 'With: '), r.helped), r.changes ? h('span', { class: 'n' }, `${r.changes} phrase${r.changes > 1 ? 's' : ''} explained`) : h('span', { class: 'n' }, 'no slang found'));
    } catch (e) { out.textContent = e.message; } finally { b.classList.remove('busy'); }
  };
}
const tryIt = debounce(async () => {
  const text = $('#tryIn').value; const out = $('#tryOut');
  if (!text.trim()) { out.innerHTML = ''; return; }
  try {
    const r = await api('glossary.test', { text });
    const before = new Set(text.split(/\s+/));
    out.innerHTML = r.fixed.split(/(\s+)/).map(w => /\s/.test(w) || before.has(w) ? esc(w) : `<mark>${esc(w)}</mark>`).join('')
      + `<span class="n">${r.changes ? r.changes + ' fix' + (r.changes > 1 ? 'es' : '') : 'nothing to fix'}${$('#glossOn').checked ? '' : ' (glossary is off)'}</span>`;
  } catch { }
}, 200);
function initGlossary() {
  customWords = chipInput($('#customWords'), { free: true, placeholder: 'Faker, Chovy, Caedrel… press Enter', onChange: saveGloss });
  customWords.set(S.gloss.custom_words);
  Object.entries(S.gloss.fixes || {}).forEach(([a, b]) => $('#fixTable').append(fixRow(a, b)));
  if (!Object.keys(S.gloss.fixes || {}).length) $('#fixTable').append(h('div', { class: 'empty' }, 'No fixes yet. Add one when Whisper keeps getting a word wrong.'));
  $('#addFix').onclick = () => { const e = $('#fixTable .empty'); if (e) e.remove(); const r = fixRow(); $('#fixTable').append(r); $('input', r).focus(); };
  initSlang();
  $('#glossOn').onchange = saveGloss; $('#coreOn').onchange = () => { $('#termCloud').classList.toggle('off', !$('#coreOn').checked); saveGloss(); };
  $('#tryIn').oninput = tryIt; tryIt();
  $('#champFilter').oninput = renderWall;
  $('#champRefresh').onclick = async () => { const b = $('#champRefresh'); b.classList.add('busy'); try { applyGloss(await api('glossary.refresh')); toast('Champion list updated', 'good', 2000); } catch (e) { toast('Couldn\'t reach Riot\'s servers: ' + e.message, 'bad'); } finally { b.classList.remove('busy'); } };
  applyGloss(S.gloss);
}
on('glossary.updated', m => applyGloss(m.glossary));

/* ================================================================ SETTINGS */
function initSettings() {
  const st = S.settings;
  $('#setChannel').value = st.my_channel || '';
  $('#setChannel').oninput = e => setSetting('my_channel', e.target.value.trim().replace(/^.*twitch\.tv\//, '').replace(/\/.*$/, ''));
  $('#setOut').value = st.out_dir; $('#setOutBtn').onclick = browseOut;
  $('#setPreload').checked = st.preload; $('#setPreload').onchange = e => setSetting('preload', e.target.checked);
  trSeg = seg($('#trSeg'), v => setSetting('translator', v)); trSeg.set(st.translator || 'better');
  $('#setNotify').checked = st.notify; $('#setNotify').onchange = e => setSetting('notify', e.target.checked);
  $('#setUpdate').checked = st.auto_update; $('#setUpdate').onchange = e => setSetting('auto_update', e.target.checked);
  $('#updBtn').onclick = async () => { const b = $('#updBtn'); b.classList.add('busy'); try { showUpdate(await api('update.check'), true); } catch (e) { toast(e.message, 'bad'); } finally { b.classList.remove('busy'); } };
  showUpdate(S.update);
  $('#setAppUpd').checked = st.app_update !== false; $('#setAppUpd').onchange = e => setSetting('app_update', e.target.checked);
  $('#appUpdBtn').onclick = async () => {
    if (S.appUpdate && S.appUpdate.newer) { installAppUpdate(); return; }
    const b = $('#appUpdBtn'); b.classList.add('busy');
    try { showAppUpdate(await api('appupdate.check'), true); } catch (e) { toast(e.message, 'bad'); } finally { b.classList.remove('busy'); }
  };
  showAppUpdate(S.appUpdate);
  const prev = st.updated_from;
  $('#rollRow').hidden = !prev; if (prev) $('#rollText').textContent = `Updated from ${prev}. Problems with this version?`;
  $('#rollBtn').onclick = rollBack;
  $('#reportBtn').onclick = () => reportProblem();
  $('#logsBtn').onclick = () => api('app.logs');
  $('#modelChip').onclick = () => showPage('settings');
  scaleSeg = seg($('#scaleSeg'), v => api('ui.scale', { value: v === 'auto' ? 'auto' : +v }));
  showScale(S.scale.value, st.ui_scale, S.scale.auto);
  renderAbout();
}
let scaleSeg;
function showScale(value, setting, auto) {
  const preset = setting === 'auto' ? 'auto' : String(+setting);
  scaleSeg.set($$('#scaleSeg button').some(b => b.dataset.v === preset) ? preset : null);
  $('#scaleNow').textContent = setting === 'auto' ? `auto · ${Math.round(auto * 100)}% for this screen` : `${Math.round(value * 100)}%`;
}
on('ui.scale', m => { S.settings.ui_scale = m.setting; S.scale = { value: m.value, auto: m.auto }; showScale(m.value, m.setting, m.auto); toast(`Interface size ${Math.round(m.value * 100)}%`, '', 1500); });
function renderAbout() {
  const a = $('#about'); if (!a) return;
  a.innerHTML = '';
  [['GPU', S.gpu || 'none found (CPU mode)'], ['Whisper', `${S.model.name} · ${S.model.state === 'ready' ? 'loaded on ' + S.model.device : S.model.state}`],
   ['Glossary', S.gloss ? `${S.champs.length} champions, ${(S.gloss.custom_words || []).length} custom words` : '—'], ['Shortcuts', 'Ctrl+1–5 switch tabs · Ctrl+Enter translate']]
    .forEach(([k, v]) => a.append(h('span', {}, k), h('b', {}, v)));
}

/* ---------- drag & drop links ---------- */
function initDrop() {
  let depth = 0;
  document.addEventListener('dragenter', e => { if ([...e.dataTransfer.types].some(t => t.startsWith('text') || t === 'Files')) { depth++; document.body.classList.add('dragging'); } });
  document.addEventListener('dragleave', () => { if (--depth <= 0) { depth = 0; document.body.classList.remove('dragging'); } });
  document.addEventListener('dragover', e => e.preventDefault());
  document.addEventListener('drop', e => {
    e.preventDefault(); depth = 0; document.body.classList.remove('dragging');
    // files: the app gets their real paths through WebView2 and answers with 'files.dropped'
    if (e.dataTransfer.files.length) {
      if (window.chrome?.webview?.postMessageWithAdditionalObjects) chrome.webview.postMessageWithAdditionalObjects('st-files', e.dataTransfer.files);
      return;
    }
    const text = (e.dataTransfer.getData('text/uri-list') || e.dataTransfer.getData('text/plain') || '').split('\n')[0].trim(); if (!text) return;
    if ($('#page-live').classList.contains('on')) { $('#liveInput').value = text; return; }
    showPage('clip'); srcMode.set('theirs'); $$('#sourceCard .mode-pane').forEach(p => p.classList.toggle('on', p.dataset.mode === 'theirs'));
    $('#theirInput').value = text; loadSource(text);
  });
}

/* ================================================================ UPDATES */
function showUpdate(u, manual) {
  if (!u) return; S.update = u; const t = $('#updText'); if (!t) return;
  if (u.error) t.textContent = `yt-dlp ${u.current || '?'} · couldn't check for updates (offline?)`;
  else if (u.updated) t.textContent = `yt-dlp ${u.latest} downloaded. Restart the app to use it.`;
  else t.textContent = `yt-dlp ${u.current || u.latest || '?'} · up to date`;
  if (manual && !u.error) toast(u.updated ? `Downloaded yt-dlp ${u.latest}. It's used next time you open the app.` : 'yt-dlp is up to date', 'good', 3000);
}
/* problems, what's new, going back */
function actionToast(msg, kind, actions, ms = 0) {
  const t = h('div', { class: 'toast upd-card ' + kind }, h('div', { class: 'upd-body' }, h('div', {}, msg),
    h('div', { class: 'row upd-btns' }, ...actions.map(([label, fn]) => h('button', { class: 'btn ghost sm', onclick: () => { t.remove(); fn(); } }, label)),
      h('button', { class: 'btn ghost sm', onclick: () => t.remove() }, 'Dismiss'))));
  $('#toasts').append(t);
  if (ms) setTimeout(() => t.remove(), ms);
}
function reportProblem(what = '') {
  api('app.report', { what }).then(() => toast('A GitHub issue opened in your browser with the version and recent log filled in. Check it, then submit.', 'good', 7000))
    .catch(e => toast(e.message, 'bad'));
}
function mdLite(md) {
  // release notes: paragraphs, "- " bullets, **bold**, `code`
  const inline = s => esc(s).replace(/\*\*(.+?)\*\*/g, '<b>$1</b>').replace(/`([^`]+)`/g, '<code>$1</code>');
  let html = '', list = false;
  for (const raw of (md || '').split('\n')) {
    const line = raw.trim();
    if (/^[-*] /.test(line)) { if (!list) { html += '<ul>'; list = true; } html += `<li>${inline(line.slice(2))}</li>`; continue; }
    if (list) { html += '</ul>'; list = false; }
    if (/^#+ /.test(line)) html += `<h4>${inline(line.replace(/^#+ /, ''))}</h4>`;
    else if (line) html += `<p>${inline(line)}</p>`;
  }
  return html + (list ? '</ul>' : '');
}
function showWhatsNew(w) {
  $('#newsTitle').textContent = `What's new in ${w.version}`;
  $('#newsFrom').textContent = `Updated from ${w.from}. Your settings, history and downloads are kept.`;
  $('#newsNotes').innerHTML = w.notes ? mdLite(w.notes) : '<p class="muted">No release notes for this version.</p>';
  $('#newsBack').hidden = !S.settings.updated_from;
  $('#newsBack').onclick = () => { $('#whatsNew').hidden = true; rollBack(); };
  $('#newsClose').onclick = () => { $('#whatsNew').hidden = true; };
  $('#whatsNew').hidden = false;
}
async function rollBack() {
  const prev = S.settings.updated_from; if (!prev) return;
  if (!confirm(`Go back to version ${prev}? The app closes, installs ${prev} and reopens. Your settings and history are kept.`)) return;
  const b = $('#rollBtn'); b.disabled = true; b.textContent = 'Downloading…';
  try { await api('appupdate.rollback'); b.textContent = 'Installing… the app will reopen'; }
  catch (e) { toast(e.message, 'bad', 8000); b.disabled = false; b.textContent = 'Go back'; }
}
on('appupdate.progress', m => { const b = $('#rollBtn'); if (b && b.disabled && !m.installing) b.textContent = `Downloading… ${Math.round(m.f * 100)}%`; });

/* app updates from GitHub releases */
let appUpdCard = null, appUpdShown = '';
function showAppUpdate(u, manual) {
  if (!u) return; S.appUpdate = u;
  const t = $('#appUpdText'), b = $('#appUpdBtn');
  if (t) {
    if (u.error) t.textContent = `StreamTranslator ${u.current} · couldn't check for updates (offline?)`;
    else if (u.newer) t.textContent = `StreamTranslator ${u.current} · version ${u.latest} is available`;
    else if (u.latest) t.textContent = `StreamTranslator ${u.current} · up to date`;
    else t.textContent = `StreamTranslator ${u.current}`;
    b.textContent = u.newer ? `Update to ${u.latest}` : 'Check now';
    b.classList.toggle('primary', !!u.newer); b.classList.toggle('ghost', !u.newer);
  }
  if (manual && u.error) toast('Couldn\'t reach GitHub: ' + u.error, 'bad');
  else if (manual && !u.newer) toast(`You have the newest version (${u.current})`, 'good', 3000);
  // after going back a version, don't pop up the version you left (Settings still offers it)
  if (u.newer && (manual || (appUpdShown !== u.latest && S.settings.skip_version !== u.latest))) appUpdPopup(u);
}
function appUpdPopup(u) {
  appUpdShown = u.latest; if (appUpdCard) appUpdCard.remove();
  const mb = u.asset && u.asset.size ? ` · ${Math.round(u.asset.size / 1048576)} MB` : '';
  const notes = (u.notes || '').split('\n').map(s => s.trim()).filter(Boolean).slice(0, 6).join('\n');
  appUpdCard = h('div', { class: 'toast upd-card' },
    h('div', { class: 'upd-body' },
      h('b', {}, `StreamTranslator ${u.latest} is out`), h('div', { class: 'muted small' }, `You have ${u.current}${mb}`),
      notes ? h('div', { class: 'upd-notes' }, notes) : '',
      h('div', { class: 'mini upd-prog', hidden: true }, h('i')),
      h('div', { class: 'row upd-btns' },
        h('button', { class: 'btn primary sm', onclick: installAppUpdate }, 'Update now'),
        h('button', { class: 'btn ghost sm', onclick: () => { appUpdCard.remove(); appUpdCard = null; } }, 'Later'),
        h('button', { class: 'btn ghost sm', onclick: () => api('os.url', { url: u.url }).catch(() => {}) }, 'What\'s new'))));
  $('#toasts').append(appUpdCard);
}
async function installAppUpdate() {
  if (!appUpdCard) appUpdPopup(S.appUpdate);
  const btns = $$('.upd-btns button', appUpdCard); btns.forEach(b => b.disabled = true);
  $('.upd-prog', appUpdCard).hidden = false; btns[0].textContent = 'Downloading…';
  try { await api('appupdate.install'); btns[0].textContent = 'Installing… the app will reopen'; }
  catch (e) { toast(e.message, 'bad', 8000); btns.forEach(b => b.disabled = false); btns[0].textContent = 'Update now'; $('.upd-prog', appUpdCard).hidden = true; }
}
on('appupdate.status', m => showAppUpdate(m));
on('appupdate.progress', m => { if (!appUpdCard) return; $('.upd-prog i', appUpdCard).style.width = Math.round(m.f * 100) + '%'; if (m.installing) $('.upd-btns button', appUpdCard).textContent = 'Installing… the app will reopen'; });

on('update.status', m => { showUpdate(m); if (m.updated) toast(`Downloaded yt-dlp ${m.latest}. Restart the app to use it.`, 'good', 6000); });

/* ================================================================ CHAT */
on('chat.status', m => {
  const t = { connected: `Connected to ${m.channel}'s chat`, loading: m.message, ready: `Chat translator ready (${m.device})`, reconnecting: 'Reconnecting to chat…',
    error: m.message, unavailable: m.message, off: 'Chat translation is off.' }[m.state];
  if (t) $('#chatStatus').textContent = t;
  if (m.state === 'error') toast(m.message, 'bad', 8000);
});
on('chat.message', m => {
  const feed = $('#chatFeed'); const near = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 60;
  feed.append(h('div', { class: 'msg' + (m.en ? ' tr' : '') },
    h('span', { class: 'u', style: `color:${m.color || '#a78bfa'}` }, m.user), h('span', { class: 't' }, m.en || m.text), m.en ? h('span', { class: 'o' }, m.text) : null));
  while (feed.childElementCount > 300) feed.firstChild.remove();
  if (near) feed.scrollTop = feed.scrollHeight; else $('#chatJump').hidden = false;
});

/* ================================================================ HISTORY */
let histFilter;
function initHistory() {
  histFilter = seg($('#histFilter'), () => renderHistory());
  $('#histSearch').oninput = debounce(searchHistory, 250);
  on('history.changed', () => { if ($('#page-history').classList.contains('on')) loadHistory(); });
}
async function loadHistory() {
  try { S.hist = await api('history.list'); } catch { S.hist = []; }
  if ($('#histSearch').value.trim()) searchHistory(); else { S.histHits = null; renderHistory(); }
}
async function searchHistory() {
  const q = $('#histSearch').value.trim();
  if (!q) { S.histHits = null; renderHistory(); return; }
  try { S.histHits = await api('history.search', { q }); } catch { S.histHits = {}; }
  S.histQ = q; renderHistory();
}
function highlight(text, q) {
  const t = esc(text); if (!q) return t;
  return t.replace(new RegExp(esc(q).replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'gi'), m => `<mark>${m}</mark>`);
}
function renderHistory() {
  const g = $('#histGrid'); g.innerHTML = '';
  const f = histFilter.get();
  let items = (S.hist || []).filter(e => f === 'all' || e.type === f);
  if (S.histHits) items = items.filter(e => S.histHits[e.id]);
  if (!items.length) { g.append(h('div', { class: 'empty' }, S.histHits ? 'Nothing matches that search.' : 'Nothing here yet. Translated clips and live sessions show up here.')); return; }
  items.forEach((e, i) => g.append(histCard(e, i)));
}
function histCard(e, i) {
  const isClip = e.type === 'clip'; const hits = S.histHits && S.histHits[e.id];
  const play = () => api('os.open', { path: isClip ? (e.subbed || e.clip) : e.transcript });
  const len = isClip ? fmt(e.end - e.start) : fmt(e.duration || 0);
  return h('div', { class: 'hcard' + (e.exists ? '' : ' missing'), style: `animation-delay:${Math.min(i, 20) * 30}ms` },
    h('div', { class: 'th', style: e.thumb_url ? `background-image:url("${e.thumb_url}")` : '', onclick: play, title: isClip ? 'Play' : 'Open transcript' },
      h('span', { class: 'type' + (isClip ? '' : ' live') }, isClip ? 'CLIP' : 'LIVE'), h('span', { class: 'len' }, len),
      h('div', { class: 'playov' }, h('i', { class: 'ic ' + (isClip ? 'ic-play' : 'ic-doc') }))),
    h('div', { class: 'bd' },
      h('b', {}, e.title || e.channel),
      h('div', { class: 'meta' }, [e.channel, ago(e.created), isClip ? (e.language_name || '') : 'live', `${e.lines || 0} lines`, e.edited ? 'edited' : ''].filter(Boolean).join(' · ') + (e.exists ? '' : ' · files moved or deleted')),
      hits && hits.snippets.length ? h('div', { class: 'snips' }, hits.snippets.map(s => h('div', { html: (s.time ? `<em>${esc(s.time)}</em>` : '') + highlight(s.text, S.histQ) }))) : null,
      h('div', { class: 'acts' },
        isClip ? h('button', { class: 'btn ghost sm', onclick: play }, 'Play') : null,
        isClip ? h('button', { class: 'btn ghost sm', onclick: () => openEditor(e.folder, e.channel) }, 'Edit subtitles') : null,
        h('button', { class: 'btn ghost sm', onclick: () => api('os.open', { path: e.transcript }) }, 'Transcript'),
        isClip ? h('button', { class: 'btn ghost sm', onclick: () => reopenClip(e) }, 'Re-open section') : null,
        h('button', { class: 'btn ghost sm', onclick: () => api('os.reveal', { path: isClip ? (e.subbed || e.clip) : e.transcript }) }, 'Folder'),
        h('button', { class: 'btn ghost sm danger', title: 'Remove from history. The files stay where they are.', onclick: async () => { await api('history.remove', { id: e.id }); loadHistory(); } }, 'Remove'))));
}
async function reopenClip(e) {
  showPage('clip'); srcMode.set('theirs'); $$('#sourceCard .mode-pane').forEach(p => p.classList.toggle('on', p.dataset.mode === 'theirs'));
  $('#theirInput').value = e.vod_url;
  try { const r = await api('source.load', { text: e.vod_url }); setVod(r.vod, [e.start, e.end]); }
  catch (err) { toast(err.message, 'bad'); }
}

/* ================================================================ SUBTITLE EDITOR */
const ED = { cues: [], folder: null, sel: -1, dirty: false, burnJob: null, position: 'Above game HUD', size: 'Medium' };
let edPos, edSize;
const fmtMs = t => { t = Math.max(0, t); const H = Math.floor(t / 3600), M = Math.floor(t % 3600 / 60), s = (t % 60).toFixed(1).padStart(4, '0'); return H ? `${H}:${String(M).padStart(2, '0')}:${s}` : `${M}:${s}`; };
const parseMs = s => { const v = parseTs(s); return v == null || isNaN(v) ? null : v; };
function initEditor() {
  edPos = seg($('#edPos'), v => { ED.position = v; paintSub(); dirtyStyle(); });
  edSize = seg($('#edSize'), v => { ED.size = v; paintSub(); dirtyStyle(); });
  $('#edClose').onclick = () => closeEditor();
  $('#edSave').onclick = () => saveSubs(true);
  $('#edBurn').onclick = burnSubs;
  $('#edSetStart').onclick = () => setCueEdge('start'); $('#edSetEnd').onclick = () => setCueEdge('end');
  $('#edSplit').onclick = splitCue; $('#edAdd').onclick = addCue;
  const v = $('#edVideo');
  v.addEventListener('timeupdate', markNow); v.addEventListener('seeked', markNow);
  const tick = () => { if ($('#editor').hidden || v.paused) { ED.ticking = false; paintSub(); return; } paintSub(); requestAnimationFrame(tick); };
  v.addEventListener('play', () => { if (!ED.ticking) { ED.ticking = true; requestAnimationFrame(tick); } });
  v.addEventListener('seeked', paintSub); v.addEventListener('pause', paintSub);
  new ResizeObserver(paintSub).observe($('.ed-video'));
  document.addEventListener('keydown', edKeys);
  $('#editor').addEventListener('mousedown', e => { if (e.target.id === 'editor') closeEditor(); });
}
async function openEditor(folder, title) {
  if (!folder) return;
  $('#editor').hidden = false; ED.folder = folder; ED.dirty = false; ED.sel = -1;
  $('#edTitle').textContent = title || 'Subtitles'; $('#edLoading').hidden = false; $('#edLoading span').textContent = 'Preparing preview…';
  $('#edBar').style.width = '0'; $('#edList').innerHTML = ''; $('#edState').textContent = '';
  const probe = document.createElement('video');
  const h264 = !!probe.canPlayType('video/mp4; codecs="avc1.42E01E, mp4a.40.2"');
  const av1 = !!probe.canPlayType('video/webm; codecs="av01.0.05M.08, opus"');
  try {
    const r = await api('subs.open', { folder, h264, av1 });
    ED.cues = r.cues.map(c => ({ ...c })); ED.meta = r.meta;
    ED.position = r.meta.position || 'Above game HUD'; ED.size = r.meta.size || 'Medium'; edPos.set(ED.position); edSize.set(ED.size);
    $('#edTitle').textContent = `${r.meta.channel} · ${fmt(r.meta.start)} – ${fmt(r.meta.end)}`;
    const vid = $('#edVideo');
    vid.poster = r.poster || '';
    ED.audioOnly = !!r.meta.audio_only;
    $('#edBurn').disabled = ED.audioOnly; $('#edBurn').title = ED.audioOnly ? 'This job only downloaded audio, so there is no video to burn into' : '';
    if (ED.audioOnly) $('#edState').textContent = 'Subtitles-only job: edit and save the .srt (no video to burn).';
    vid.onloadeddata = () => { $('#edLoading').hidden = true; paintSub(); };
    vid.onerror = () => { $('#edLoading span').textContent = "This video can't play in the editor, but you can still edit the text."; };
    vid.src = r.video;
    renderCues();
  } catch (e) { toast(e.message, 'bad', 8000); closeEditor(true); }
}
on('subs.progress', m => { $('#edBar').style.width = Math.round(m.f * 100) + '%'; $('#edLoading span').textContent = `Preparing preview… ${Math.round(m.f * 100)}%`; });
function closeEditor(force) {
  if (!force && ED.dirty && !confirm('Close without saving your edits?')) return;
  $('#editor').hidden = true; const v = $('#edVideo'); v.pause(); v.removeAttribute('src'); v.load(); ED.dirty = false;
}
function dirty() { ED.dirty = true; $('#edState').textContent = 'Unsaved changes'; }
function dirtyStyle() { $('#edState').textContent = 'Position/size apply when you burn the video'; }
function renderCues() { const list = $('#edList'); list.innerHTML = ''; ED.cues.forEach((c, i) => list.append(cueEl(c, i))); markNow(); }
function cueEl(c, i) {
  const s = h('input', { value: fmtMs(c.start), title: 'Start' }), e = h('input', { value: fmtMs(c.end), title: 'End' }), ta = h('textarea', { rows: 2 });
  ta.value = c.text;
  const row = h('div', { class: 'cue' + (i === ED.sel ? ' sel' : ''), 'data-i': i, style: c.color ? `box-shadow: inset 3px 0 0 ${c.color}` : null, title: c.color ? 'Another speaker (shown in this colour)' : null }, h('div', { class: 'tm' }, s, e), ta, h('button', { class: 'del', title: 'Delete line' }, '✕'));
  row.addEventListener('mousedown', ev => { if (ev.target.closest('.del')) return; selectCue(i, !['TEXTAREA', 'INPUT'].includes(ev.target.tagName)); });
  ta.addEventListener('focus', () => selectCue(i, false));
  ta.oninput = () => { c.text = ta.value; dirty(); };
  const tchange = (inp, key) => { const v = parseMs(inp.value); if (v == null) { inp.classList.add('bad'); return; } inp.classList.remove('bad'); c[key] = v; if (c.end <= c.start) c.end = c.start + 0.5; dirty(); };
  s.onchange = () => tchange(s, 'start'); e.onchange = () => tchange(e, 'end');
  $('.del', row).onclick = () => { ED.cues.splice(i, 1); ED.sel = Math.min(ED.sel, ED.cues.length - 1); dirty(); renderCues(); };
  return row;
}
function selectCue(i, seek) {
  ED.sel = i; $$('.cue').forEach(r => r.classList.toggle('sel', +r.dataset.i === i));
  if (seek && ED.cues[i]) $('#edVideo').currentTime = ED.cues[i].start + 0.01;
}
function markNow() {
  const v = $('#edVideo'); const t = v.currentTime || 0; const i = ED.cues.findIndex(c => t >= c.start && t < c.end);
  $$('.cue').forEach(r => r.classList.toggle('now', +r.dataset.i === i));
  if (i >= 0 && !v.paused) { const row = $(`.cue[data-i="${i}"]`); const list = $('#edList');
    if (row && (row.offsetTop < list.scrollTop || row.offsetTop > list.scrollTop + list.clientHeight - row.offsetHeight)) row.scrollIntoView({ block: 'center', behavior: 'smooth' }); }
}
function paintSub() {
  const v = $('#edVideo'), box = $('.ed-video'), sub = $('#edSub'); if (!box) return;
  const t = v.currentTime || 0; const c = ED.cues.find(c => t >= c.start && t < c.end);
  const text = c ? c.text : ''; if (sub.textContent !== text) sub.textContent = text;
  sub.style.color = (c && c.color) || '';
  const H = box.clientHeight; const size = { Small: 12, Medium: 14, Large: 17 }[ED.size] || 14;
  const [align, margin] = { 'Above game HUD': [2, 46], Bottom: [2, 12], Top: [8, 14] }[ED.position] || [2, 46];
  sub.style.fontSize = (H * size / 288 * 1.1) + 'px';
  if (align === 8) { sub.style.top = (H * margin / 288) + 'px'; sub.style.bottom = 'auto'; } else { sub.style.bottom = (H * margin / 288) + 'px'; sub.style.top = 'auto'; }
}
function needSel() { if (ED.sel < 0 || !ED.cues[ED.sel]) { toast('Click a line first.', 'warn', 2000); return null; } return ED.cues[ED.sel]; }
function setCueEdge(key) {
  const c = needSel(); if (!c) return; const t = $('#edVideo').currentTime;
  c[key] = t; if (c.end <= c.start) { if (key === 'start') c.end = t + 1; else c.start = Math.max(0, t - 1); }
  dirty(); const keep = ED.sel; renderCues(); selectCue(keep, false);
}
function splitCue() {
  const c = needSel(); if (!c) return; const t = $('#edVideo').currentTime;
  if (t <= c.start + 0.2 || t >= c.end - 0.2) { toast('Move the playhead inside the selected line to split it.', 'warn', 2500); return; }
  const words = c.text.split(/\s+/); const k = clamp(Math.round(words.length * (t - c.start) / (c.end - c.start)), 1, Math.max(1, words.length - 1));
  const second = { start: t, end: c.end, text: words.slice(k).join(' '), ...(c.color ? { color: c.color } : {}) };
  c.end = t; c.text = words.slice(0, k).join(' ');
  ED.cues.splice(ED.sel + 1, 0, second); dirty(); const keep = ED.sel + 1; renderCues(); selectCue(keep, false);
}
function addCue() {
  const t = $('#edVideo').currentTime || 0; let i = ED.cues.findIndex(c => c.start > t); if (i < 0) i = ED.cues.length;
  ED.cues.splice(i, 0, { start: t, end: t + 2, text: '' }); dirty(); renderCues(); selectCue(i, false);
  const ta = $(`.cue[data-i="${i}"] textarea`); if (ta) { ta.focus(); ta.scrollIntoView({ block: 'center' }); }
}
function edKeys(e) {
  if ($('#editor').hidden) return;
  if (e.key === 'Escape') { e.preventDefault(); closeEditor(); return; }
  if (e.ctrlKey && e.key.toLowerCase() === 's') { e.preventDefault(); saveSubs(true); return; }
  if (['INPUT', 'TEXTAREA'].includes(e.target.tagName)) return;
  const v = $('#edVideo');
  if (e.key === ' ') { e.preventDefault(); v.paused ? v.play() : v.pause(); }
  else if (e.key === 'ArrowLeft') { e.preventDefault(); v.currentTime = Math.max(0, v.currentTime - 2); }
  else if (e.key === 'ArrowRight') { e.preventDefault(); v.currentTime = v.currentTime + 2; }
  else if (e.key === '[') setCueEdge('start');
  else if (e.key === ']') setCueEdge('end');
}
async function saveSubs(announce) {
  try {
    await api('subs.save', { folder: ED.folder, cues: ED.cues });
    ED.dirty = false; $('#edState').textContent = 'Saved'; if (announce) toast('Subtitles saved (the original is kept as a backup).', 'good', 2500);
    return true;
  } catch (e) { toast(e.message, 'bad'); return false; }
}
async function burnSubs() {
  if (ED.burnJob) return;
  if (!(await saveSubs(false))) return;
  const id = await api('subs.burn', { folder: ED.folder, position: ED.position, size: ED.size });
  ED.burnJob = id; Object.assign(J(id), { steps: ['Burning subtitles into video'], label: 'Re-burn edited subtitles' });
  $('#edBurn').classList.add('busy'); $('#edState').textContent = 'Burning… (waits for other jobs in the queue)';
}
on('clip.progress', m => { if (m.job === ED.burnJob && m.f >= 0) $('#edState').textContent = `Burning… ${Math.round(m.f * 100)}%`; });
function onBurnDone(m, err) {
  const id = (m && m.job) || (err && err.job); if (!ED.burnJob || id !== ED.burnJob) return;
  ED.burnJob = null; $('#edBurn').classList.remove('busy');
  if (m) {
    const st = $('#edState'); st.innerHTML = ''; st.append('Video updated ✓ ', h('button', { class: 'btn ghost sm', onclick: () => api('os.open', { path: m.subbed }) }, 'Play'));
    if (S.lastResult && S.lastResult.folder === m.folder) S.lastResult.subbed = m.subbed;
  } else $('#edState').textContent = 'Burning failed or was cancelled.';
}

/* ================================================================ SETUP / COMPONENTS */
const SETUP = { st: null, busy: {} };
function initSetup(comp, needed) {
  SETUP.st = comp;
  $('#openSetup').onclick = () => openSetup();
  $('#setupClose').onclick = async () => { $('#setup').hidden = true; await api('setup.done'); };
  $('#setupAll').onclick = setupAll;
  if (needed) openSetup();
}
async function openSetup() {
  $('#setup').hidden = false;
  try { SETUP.st = await api('components.status'); } catch { }
  renderSetup();
}
function srow(id, ok, title, sub, acts, need) {
  const busy = SETUP.busy[id];
  return h('div', { class: 'srow ' + (ok ? 'ok' : need ? 'need' : ''), id: 'srow-' + id },
    h('span', { class: 'st' }, ok ? '✓' : need ? '!' : '–'),
    h('div', {}, h('b', {}, title), h('div', { class: 'sub' }, busy ? busy.text : sub),
      busy ? h('div', { class: 'bar' }, h('div', { class: 'bar-fill', style: `width:${Math.round(busy.f * 100)}%` })) : null),
    h('div', { class: 'acts' }, busy ? null : acts));
}
function dl(what, name, label) {
  return h('button', { class: 'btn primary sm', onclick: () => startDl(what, name) }, label);
}
function startDl(what, name) {
  const id = what + (name ? '-' + name : '');
  SETUP.busy[id] = { f: 0, text: 'Starting…' }; renderSetup();
  api('components.download', { what, name });
}
on('comp.progress', m => { const id = m.what + (m.name ? '-' + m.name : ''); SETUP.busy[id] = { f: Math.max(0, m.f), text: m.text }; renderSetupRow(id); });
on('comp.done', m => { delete SETUP.busy[m.what + (m.name ? '-' + m.name : '')]; SETUP.st = m.status; renderSetup(); toast(`${m.what === 'gpu' ? 'GPU libraries' : 'Download'} ready`, 'good', 2500);
  if (m.what === 'whisper' || m.what === 'gpu') api('model.preload', { name: S.settings.model }).catch(() => { }); });
on('comp.error', m => { delete SETUP.busy[m.what + (m.name ? '-' + m.name : '')]; renderSetup(); toast(m.message, 'bad', 9000); });
function renderSetupRow(id) {
  const el = $('#srow-' + id), b = SETUP.busy[id]; if (!el || !b) return renderSetup();
  const sub = $('.sub', el); if (sub) sub.textContent = b.text;
  const fill = $('.bar-fill', el); if (fill) fill.style.width = Math.round(b.f * 100) + '%'; else renderSetup();
}
function renderSetup() {
  const st = SETUP.st; if (!st) return;
  const L = $('#setupList'); L.innerHTML = '';
  const g = st.gpu;
  L.append(srow('gpu', g.ok, 'NVIDIA GPU libraries (cuBLAS + cuDNN)',
    !st.has_nvidia ? 'No NVIDIA GPU found. Whisper will run on the CPU, which is much slower.' :
      g.ok ? `Ready: ${g.source}${g.dir ? ' · ' + g.dir : ''}` : 'Needed to run Whisper on your GPU. About 1.3 GB from NVIDIA (via PyPI), checksum-verified.',
    g.ok || !st.has_nvidia ? null : dl('gpu', '', 'Download 1.3 GB'), st.has_nvidia && !g.ok));
  const w = st.models.whisper, sel = st.selected_model;
  const wOk = sel === 'custom' ? !!st.custom_model : !!(w[sel] && w[sel].ok);
  L.append(srow('whisper-' + sel, wOk, 'Whisper speech model',
    sel === 'custom' ? `Using your folder: ${st.custom_model}` :
      Object.entries(w).map(([k, v]) => `${v.label}${v.ok ? ' ✓' : ` (${v.gb} GB)`}`).join(' · ') + ` · selected: ${(w[sel] || {}).label || sel}`,
    [!wOk && w[sel] ? dl('whisper', sel, `Download ${w[sel].label} (${w[sel].gb} GB)`) : null,
     !w['large-v3-turbo'].ok ? h('button', { class: 'btn ghost sm', onclick: () => startDl('whisper', 'large-v3-turbo') }, 'Add Turbo (1.6 GB)') : null,
     h('button', { class: 'btn ghost sm', title: 'A faster-whisper (CTranslate2) model folder from another tool', onclick: pickModelFolder }, 'Use a folder I have')], !wOk));
  const t = st.models.translator, tr = st.translator;
  L.append(srow('translator-' + tr, t[tr].ok, `Text translator (${tr === 'better' ? 'Better, 1.3B' : 'Standard, 600M'})`,
    t[tr].ok ? 'Ready. Used for two-step translation and chat.' : `For two-step translation and chat. ${t[tr].gb} GB. You can switch size in Settings.`,
    t[tr].ok ? null : dl('translator', tr, `Download ${t[tr].gb} GB`), !t[tr].ok));
  const iso = st.models.isolation;
  L.append(srow('isolation', iso.ok, 'Voice isolation model (optional)',
    iso.ok ? 'Ready.' : 'Removes game sound and music before Whisper listens. 66 MB.', iso.ok ? null : dl('isolation', '', 'Download 66 MB'), false));
  const f = st.ffmpeg;
  L.append(srow('ffmpeg', f.ok, 'FFmpeg (video and audio)', f.ok ? `Ready: ${f.source}` : 'Not found. Install it with: winget install Gyan.FFmpeg',
    f.ok ? null : h('button', { class: 'btn ghost sm', onclick: () => { api('os.copy', { text: 'winget install Gyan.FFmpeg' }); toast('Command copied', 'good', 1500); } }, 'Copy command'), !f.ok));
}
async function pickModelFolder() {
  try { const st = await api('components.model_folder'); if (st) { SETUP.st = st; S.settings.model = 'custom'; S.settings.custom_model = st.custom_model; renderSetup(); toast('Using your model folder', 'good', 2500); } }
  catch (e) { toast(e.message, 'bad', 9000); }
}
function setupAll() {
  const st = SETUP.st; if (!st) return;
  if (st.has_nvidia && !st.gpu.ok && !SETUP.busy.gpu) startDl('gpu', '');
  const sel = st.selected_model;
  if (sel !== 'custom' && st.models.whisper[sel] && !st.models.whisper[sel].ok) startDl('whisper', sel);
  if (!st.models.translator[st.translator].ok) startDl('translator', st.translator);
}
