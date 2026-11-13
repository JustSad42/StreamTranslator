// First run on a "new PC": start the app with empty APPDATA / LOCALAPPDATA / model cache folders (see runui.ps1),
// check the setup screen appears, then download the GPU libraries and the small Whisper model through it.
//   node firstrun.mjs 9333 <out_dir>
import fs from 'node:fs';

const [port = '9333', out = '.'] = process.argv.slice(2);
fs.mkdirSync(out, { recursive: true });
const t = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find(x => x.type === 'page' && x.url.includes('tauri'));
const ws = new WebSocket(t.webSocketDebuggerUrl); await new Promise(r => ws.onopen = r);
let seq = 0; const p = new Map(); ws.onmessage = m => { const d = JSON.parse(m.data); if (p.has(d.id)) { p.get(d.id)(d); p.delete(d.id); } };
const send = (method, params = {}) => new Promise(r => { const id = ++seq; p.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const js = async e => {
  const r = await send('Runtime.evaluate', { expression: `(async () => { ${e} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description);
  return r.result?.result?.value;
};
const shot = async n => { const r = await send('Page.captureScreenshot', { format: 'png' }); fs.writeFileSync(`${out}/${n}.png`, Buffer.from(r.result.data, 'base64')); };
const sleep = ms => new Promise(r => setTimeout(r, ms));
let ok = true;
const log = (pass, name, detail) => { ok &&= pass; console.log(`${pass ? 'PASS' : 'FAIL'} ${name}: ${detail}`); };

for (let i = 0; i < 100 && !(await js('return !!S.settings')); i++) await sleep(200);
await sleep(1500);
const setup = await js(`return !document.getElementById('setup').hidden`);
const st0 = await js(`return api('components.status')`);
await shot('f1-setup');
log(setup, 'setup screen shows on first run', `gpu: ${st0.gpu.ok ? 'ok' : 'missing'}, ffmpeg: ${st0.ffmpeg.ok}, whisper small: ${JSON.stringify(st0.models).slice(0, 60)}`);

async function download(what, name) {
  await js(`window.__c = null; on('comp.done', m => window.__c = m); on('comp.error', m => window.__c = m)`);
  const t0 = Date.now();
  await js(`return api('components.download', { what: ${JSON.stringify(what)}, name: ${JSON.stringify(name)} })`);
  let last = '';
  for (let i = 0; i < 1800; i++) {
    const c = await js('return window.__c');
    if (c) return { c, s: (Date.now() - t0) / 1000 };
    if (i % 30 === 0) { const row = await js(`return document.getElementById('setupList')?.innerText.replace(/\\s+/g, ' ').slice(0, 160)`); if (row !== last) { console.log('   ', row); last = row; } }
    await sleep(1000);
  }
  return { c: { message: 'timed out' }, s: (Date.now() - t0) / 1000 };
}

let r = await download('gpu', '');
log(r.c.type === 'comp.done' && r.c.status.gpu.ok, 'GPU libraries download', `${r.s.toFixed(0)} s; ${r.c.message || JSON.stringify(r.c.status?.gpu).slice(0, 120)}`);
r = await download('whisper', 'small');
log(r.c.type === 'comp.done', 'Whisper small download', `${r.s.toFixed(0)} s; ${r.c.message || 'ok'}`);
await shot('f2-setup-done');

// the downloaded model loads on the GPU with the downloaded libraries
await js(`window.__m = []; on('model.status', m => window.__m.push(m))`);
await js(`return api('settings.save', { data: { model: 'small' } })`);
await js(`return api('model.preload', { name: 'small' })`).catch(e => console.log('   model.load:', e.message));
let ms;
for (let i = 0; i < 120; i++) { ms = (await js('return window.__m')).find(m => m.state === 'ready' || m.state === 'error'); if (ms) break; await sleep(500); }
log(ms?.state === 'ready' && ms.device === 'GPU', 'model loads on the GPU', JSON.stringify(ms));
process.exit(ok ? 0 : 1);
