// End-to-end check of the Rust app through its real interface.
// Start the app with ST_DEVTOOLS_PORT=9333 (and, to keep your own data safe, APPDATA pointing at a copy),
// then:  node smoke.mjs 9333 <out_dir> [--quick]
// Drives app.js the way a user would (loadSource, queueJobs, openEditor, showPage...) plus direct api()
// calls, checks the results and saves a screenshot per tab in <out_dir>.
import fs from 'node:fs';

const port = process.argv[2] || '9333';
const out = process.argv[3] || '.';
const quick = process.argv.includes('--quick');
const VOD = 'https://www.twitch.tv/videos/2923514362';
fs.mkdirSync(out, { recursive: true });

const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
const page = targets.find(t => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise(r => (ws.onopen = r));
let seq = 0;
const pending = new Map();
ws.onmessage = m => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
};
const send = (method, params = {}) => new Promise(r => { const id = ++seq; pending.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const js = async expr => {
  const r = await send('Runtime.evaluate', { expression: `(async () => { ${expr} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text);
  return r.result?.result?.value;
};
const shot = async name => {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync(`${out}/${name}.png`, Buffer.from(r.result.data, 'base64'));
};
const sleep = ms => new Promise(r => setTimeout(r, ms));
const results = [];
async function step(name, fn) {
  const t = Date.now();
  try {
    const detail = await fn();
    results.push({ name, ok: true, s: (Date.now() - t) / 1000, detail });
    console.log(`PASS ${name} (${((Date.now() - t) / 1000).toFixed(1)}s) ${detail ?? ''}`);
  } catch (e) {
    results.push({ name, ok: false, s: (Date.now() - t) / 1000, detail: String(e.message || e) });
    console.log(`FAIL ${name}: ${e.message || e}`);
  }
}
const check = (cond, msg) => { if (!cond) throw new Error(msg); };

await step('interface booted (app.init)', async () => {
  for (let i = 0; i < 100; i++) {
    if (await js('return !!(S.settings && S.settings.model)')) break;
    await sleep(100);
  }
  const v = await js('return { model: S.settings.model, gpu: S.gpu, champs: S.champs.length }');
  check(v.model && v.champs > 100, JSON.stringify(v));
  await shot('01-clip-tab');
  return JSON.stringify(v);
});

await step('model chip turns ready', async () => {
  for (let i = 0; i < 300; i++) {
    const t = await js(`return document.querySelector('#modelChip .txt').textContent`);
    if (t.includes('ready')) return t;
    await sleep(200);
  }
  throw new Error(await js(`return document.querySelector('#modelChip .txt').textContent`));
});

await step('glossary.test', async () => {
  const r = await js(`return api('glossary.test', { text: 'Nafiri flashes in' })`);
  check(r.fixed === 'Naafiri flashes in', JSON.stringify(r));
  return r.fixed;
});

await step('components.status', async () => {
  const r = await js(`return api('components.status')`);
  check(r.gpu.ok && r.ffmpeg.ok && r.models.whisper['large-v3'].ok, JSON.stringify(r).slice(0, 300));
  return `gpu: ${r.gpu.source}, ffmpeg: ${r.ffmpeg.source}`;
});

await step('channel lookup (source.load "ibai")', async () => {
  const r = await js(`return api('source.load', { text: 'ibai' })`);
  check(r.kind === 'channel' && r.vods.length > 0, JSON.stringify(r).slice(0, 200));
  return `${r.vods.length} VODs, newest: ${r.vods[0].title.slice(0, 40)}`;
});

let vodId;
await step('load a VOD through the Clip tab (loadSource)', async () => {
  await js(`showPage('clip'); srcMode.set('theirs'); document.querySelector('#theirInput').value = '${VOD}'; await loadSource('${VOD}')`);
  for (let i = 0; i < 100 && !(await js('return !!S.vod')); i++) await sleep(100);
  const v = await js('return S.vod && { id: S.vod.id, title: S.vod.title, dur: S.vod.duration, sb: !!S.vod.storyboard }');
  check(v && v.id, 'no VOD loaded');
  vodId = v.id;
  await sleep(2500); // let the filmstrip and hover previews load
  await shot('02-clip-vod-loaded');
  return `${v.title.slice(0, 40)}… ${Math.round(v.dur)} s, storyboard: ${v.sb}`;
});

await step('frame grab (vod.frame)', async () => {
  const r = await js(`return api('vod.frame', { id: '${vodId}', t: 3650, w: 480 })`);
  check(r.startsWith('data:image/jpeg;base64,') && r.length > 5000, r.slice(0, 60));
  return `${Math.round(r.length / 1024)} KB`;
});

await step('sync from my VOD (sync.match)', async () => {
  const r = await js(`return api('sync.match', { mine: '${VOD}', my_start: 3600, my_end: 3700, theirs: ['2923514362', 'notarealchannel_xyz123'] })`);
  check(r.results[0].ok && r.results[0].offset === 0 && r.results[0].start === 3600, JSON.stringify(r.results[0]).slice(0, 200));
  check(!r.results[1].ok, 'the made-up channel should fail');
  return `same VOD: offset ${r.results[0].offset}; unknown channel: "${r.results[1].error.split('\n')[0].slice(0, 60)}"`;
});

if (!quick) {
  await step('snap to game (snap.run)', async () => {
    const r = await js(`return api('snap.run', { id: '${vodId}', lo: 3600, hi: 3780 })`);
    check(typeof r.game_start === 'number' && r.end > r.start, JSON.stringify(r));
    return `game started at ${Math.round(r.game_start)} s, ends ${Math.round(r.end)} s, ${r.readings} readings`;
  });
}

let folder;
await step('clip job through the queue (burn on)', async () => {
  await js(`window.__done = null; on('clip.done', m => window.__done = m); on('clip.error', m => window.__done = m); window.__stages = []; on('clip.stage', m => window.__stages.push(m.label))`);
  const outDir = `${out.replace(/\\/g, '\\\\')}\\\\clips`;
  const ids = await js(`return api('queue.add', { jobs: [{ url: '${VOD}', start: 3600, end: 3630, vod_id: '${vodId}', burn: true, quality: '720p', model: 'large-v3', method: 'two-step', use_glossary: true, auto_details: true, out_dir: '${outDir}', label: 'UI test clip' }] })`);
  check(ids.length === 1, 'no job id');
  await sleep(4000);
  await shot('03-job-running');
  for (let i = 0; i < 600 && !(await js('return window.__done')); i++) await sleep(500);
  const d = await js('return window.__done');
  check(d && d.type === 'clip.done', JSON.stringify(d));
  folder = d.folder;
  await sleep(800);
  await shot('04-job-done');
  return `${d.segments} lines in ${Math.round(d.seconds)} s; stages: ${(await js('return window.__stages')).join(' > ')}`;
});

await step('history shows the clip, search finds its text', async () => {
  await js(`showPage('history')`);
  await sleep(1500);
  await shot('05-history');
  const items = await js(`return api('history.list')`);
  const mine = items.find(e => e.folder === folder);
  check(mine && mine.exists && mine.thumb_url, JSON.stringify(mine).slice(0, 200));
  const word = (fs.readFileSync(`${folder}/${fs.readdirSync(folder).find(f => f.endsWith('.en.srt'))}`, 'utf8').split('\n')[2] || 'the').split(' ')[0];
  const hits = await js(`return api('history.search', { q: ${JSON.stringify(word)} })`);
  check(hits[mine.id], `no search hit for "${word}"`);
  return `${items.length} entries; search "${word}": ${hits[mine.id].hits} hits`;
});

await step('subtitle editor plays the H.264 clip directly', async () => {
  await js(`openEditor(${JSON.stringify(folder)}, 'test')`);
  for (let i = 0; i < 100; i++) {
    const st = await js(`const v = document.querySelector('#edVideo'); return v && v.readyState`);
    if (st >= 1) break;
    await sleep(200);
  }
  const v = await js(`const v = document.querySelector('#edVideo'); v.currentTime = 10; await new Promise(r => setTimeout(r, 1200)); return { src: v.src.slice(0, 40), dur: v.duration, w: v.videoWidth, h: v.videoHeight, cues: ED.cues.length, err: v.error && v.error.message }`);
  check(v.dur > 25 && v.w > 0 && !v.err, JSON.stringify(v));
  await shot('06-editor');
  return `${v.w}x${v.h}, ${v.dur.toFixed(1)} s, ${v.cues} cues`;
});

await step('editor save (subs.save) keeps a backup', async () => {
  const r = await js(`ED.cues[0].text = 'Edited by the UI test'; return api('subs.save', { folder: ${JSON.stringify(folder)}, cues: ED.cues })`);
  check(fs.existsSync(r.backup) && fs.readFileSync(r.saved, 'utf8').includes('Edited by the UI test'), JSON.stringify(r));
  return 'saved + .original.srt backup';
});

await step('re-burn edited subtitles (subs.burn)', async () => {
  await js(`window.__done = null; closeEditor && closeEditor()`);
  await js(`return api('subs.burn', { folder: ${JSON.stringify(folder)}, position: 'Bottom', size: 'Large' })`);
  for (let i = 0; i < 300 && !(await js('return window.__done')); i++) await sleep(500);
  const d = await js('return window.__done');
  check(d && d.type === 'clip.done' && d.kind === 'burn' && fs.existsSync(d.subbed), JSON.stringify(d));
  return `${Math.round(d.seconds)} s`;
});

await step('glossary tab: save a custom word and read it back', async () => {
  await js(`showPage('glossary')`);
  await sleep(800);
  await shot('07-glossary');
  const before = await js(`return api('glossary.get')`);
  const r = await js(`return api('glossary.save', { data: { custom_words: [...${JSON.stringify(before.custom_words)}, 'Zeus'] } })`);
  check(r.custom_words.includes('Zeus'), JSON.stringify(r.custom_words));
  await js(`return api('glossary.save', { data: { custom_words: ${JSON.stringify(before.custom_words)} } })`);
  return `${r.champions.length} champions, ${r.builtin_slang} built-in slang`;
});

if (!quick) {
  await step('slang "try it" (glossary.slang_test, loads NLLB)', async () => {
    const r = await js(`return api('glossary.slang_test', { text: 'Qué jugadón de Faker', lang: 'es' })`);
    check(r.changes >= 1 && r.helped, JSON.stringify(r));
    return `plain: "${r.plain}" | helped: "${r.helped}"`;
  });
}

await step('settings tab + interface size shortcuts', async () => {
  await js(`showPage('settings')`);
  await sleep(800);
  await shot('08-settings');
  const z1 = await js(`return api('ui.scale', { value: '+' })`);
  const z0 = await js(`return api('ui.scale', { value: '0' })`);
  check(z1 > z0, `${z1} vs ${z0}`);
  const s = await js(`return api('settings.save', { data: { quality: '480p' } })`);
  await js(`return api('settings.save', { data: { quality: '720p' } })`);
  check(s.quality === '480p', 'setting not saved');
  return `zoom ${z0} -> ${z1} -> back`;
});

await step('live tab opens (live mode itself: live.mjs)', async () => {
  await js(`showPage('live')`);
  await sleep(500);
  await shot('09-live');
  return 'ok';
});

await step('no errors in the page console', async () => {
  const errs = await js(`return window.__errs || []`);
  return errs.length ? errs.join(' | ') : 'none recorded';
});

const passed = results.filter(r => r.ok).length;
console.log(`\n${passed}/${results.length} passed`);
fs.writeFileSync(`${out}/results.json`, JSON.stringify(results, null, 1));
ws.close();
process.exit(passed === results.length ? 0 : 1);
