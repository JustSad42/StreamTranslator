// Live mode through the real interface: start a live channel from the Live tab, wait for captions,
// show the floating caption window, read captions from the OBS source, check chat, stop, and check
// History and the saved transcript.
// Start the app with ST_DEVTOOLS_PORT=9333 (APPDATA pointing at a copy), then:
//   node live.mjs 9333 <out_dir> <live channel> [seconds]
import fs from 'node:fs';

const port = process.argv[2] || '9333';
const out = process.argv[3] || '.';
const channel = process.argv[4] || 'ibai';
const seconds = +(process.argv[5] || 90);
fs.mkdirSync(out, { recursive: true });

async function connect(match) {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
  const t = targets.find(match);
  if (!t) return null;
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise(r => (ws.onopen = r));
  let seq = 0;
  const pending = new Map();
  ws.onmessage = m => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); } };
  const send = (method, params = {}) => new Promise(r => { const id = ++seq; pending.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
  const js = async expr => {
    const r = await send('Runtime.evaluate', { expression: `(async () => { ${expr} })()`, awaitPromise: true, returnByValue: true });
    if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text);
    return r.result?.result?.value;
  };
  const shot = async name => { const r = await send('Page.captureScreenshot', { format: 'png' }); fs.writeFileSync(`${out}/${name}.png`, Buffer.from(r.result.data, 'base64')); };
  return { ws, js, shot, url: t.url };
}
const main = await connect(t => t.type === 'page' && !t.url.includes('overlay'));
const { js, shot } = main;
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

for (let i = 0; i < 100 && !(await js('return !!(S.settings && S.settings.model)')); i++) await sleep(100);
await js(`window.__ev = []; ['live.status','live.caption','live.partial','live.lang','live.error','live.frame','live.level','chat.status','chat.message','history.changed'].forEach(t => on(t, m => window.__ev.push({ t, at: Date.now(), m: t === 'live.frame' ? { len: m.data.length } : m })))`);
const events = t => js(`return window.__ev.filter(e => e.t === ${JSON.stringify(t)}).map(e => e.m)`);

await step('live tab: Go live', async () => {
  await js(`showPage('live'); $('#liveInput').value = ${JSON.stringify(channel)}; $('#liveChat').checked = true; toggleLive()`);
  for (let i = 0; i < 300; i++) {
    const st = (await events('live.status')).map(m => m.state);
    if (st.includes('live')) return st.join(' > ');
    const err = await events('live.error');
    if (err.length) throw new Error(err[0].message);
    await sleep(200);
  }
  throw new Error('never went live');
});

let caps = [];
await step(`captions for ${seconds} s`, async () => {
  const t0 = Date.now();
  while (Date.now() - t0 < seconds * 1000) {
    await sleep(5000);
    if (Date.now() - t0 > 30000 && !(await js(`return document.getElementById('ovTog').checked`))) {
      await js(`const t = document.getElementById('ovTog'); t.checked = true; t.onchange({ target: t })`); // caption window on, as a user would
    }
  }
  caps = await events('live.caption');
  const partials = (await events('live.partial')).filter(p => p.text).length;
  const lang = (await events('live.lang')).pop();
  await shot('l1-live-tab');
  check(caps.length >= 3, `${caps.length} captions`);
  const lag = caps.map(c => c.delay + c.took);
  return `${caps.length} captions, ${partials} drafts, language ${lang?.name}${lang?.locked ? ' (locked)' : ''}, delay avg ${(lag.reduce((a, b) => a + b, 0) / lag.length).toFixed(1)} s; last: "${caps.at(-1).text}"`;
});

await step('stream preview + level meter', async () => {
  const frames = await events('live.frame'), levels = await events('live.level');
  check(frames.length >= 1 && levels.length > 50, `${frames.length} frames, ${levels.length} levels`);
  return `${frames.length} preview frames, ${levels.length} meter updates`;
});

await step('floating caption window shows the captions', async () => {
  const ov = await connect(t => t.url.includes('overlay'));
  check(ov, 'no caption window');
  await sleep(6000);
  const st = await ov.js(`return { text: [...document.querySelectorAll('.t')].map(e => e.textContent).join(' '), size: getComputedStyle(document.documentElement).getPropertyValue('--size'), w: innerWidth, h: innerHeight }`);
  await ov.shot('l2-caption-window');
  ov.ws.close();
  check(st.text && st.text !== 'Captions will appear here', JSON.stringify(st));
  return `${st.w}x${st.h}, text ${st.size}: "${st.text.slice(0, 60)}"`;
});

await step('OBS browser source gets captions', async () => {
  const url = await js(`return api('obs.url')`);
  const page = await (await fetch(url)).text();
  check(page.includes('EventSource'), 'page');
  const ctl = new AbortController();
  const res = await fetch(url + 'events', { signal: ctl.signal });
  const reader = res.body.getReader();
  let got = '';
  const t0 = Date.now();
  while (Date.now() - t0 < 40000 && !got.includes('data:')) {
    const r = await Promise.race([reader.read(), sleep(40000).then(() => ({ done: true }))]);
    if (r.done) break;
    got += new TextDecoder().decode(r.value);
  }
  ctl.abort();
  const line = got.split('\n').find(l => l.startsWith('data:'));
  check(line, `nothing in 40 s: ${got.slice(0, 80)}`);
  return `${url} -> ${line.slice(0, 90)}`;
});

await step('chat: connects, translates', async () => {
  const st = (await events('chat.status')).map(m => m.state);
  const msgs = await events('chat.message');
  check(st.includes('connected'), st.join(','));
  const tr = msgs.filter(m => m.en);
  await js(`showPage('live')`);
  return `${st.join(' > ')}; ${msgs.length} messages, ${tr.length} translated${tr.length ? `, e.g. "${tr[0].text}" -> "${tr[0].en}"` : ''}`;
});

await step('change a setting while live (live.update)', async () => {
  const r = await js(`return api('live.update', { orig: false })`);
  check(r === true, String(r));
  return 'ok';
});

let stopped;
await step('stop: transcript saved, History entry', async () => {
  await js(`toggleLive()`);
  for (let i = 0; i < 150 && !stopped; i++) { stopped = (await events('live.status')).find(m => m.state === 'stopped'); await sleep(200); }
  check(stopped, 'no stopped event');
  const tx = fs.readFileSync(stopped.transcript, 'utf8');
  const srt = fs.readFileSync(stopped.transcript.replace('.transcript.txt', '.en.srt'), 'utf8');
  await sleep(500);
  const h = (await js(`return api('history.list')`)).find(e => e.type === 'live' && e.transcript === stopped.transcript);
  check(h && h.lines === stopped.lines, JSON.stringify(h).slice(0, 200));
  check(tx.split('\n').filter(l => l.startsWith('[')).length === stopped.lines, 'transcript lines');
  await js(`showPage('history')`);
  await sleep(1200);
  await shot('l3-history');
  return `${stopped.lines} lines; transcript ${tx.length} chars, SRT ${srt.split('-->').length - 1} cues; history thumb ${h.thumb ? 'yes' : 'no'}`;
});

await step('chat stops with live', async () => {
  const n = (await events('chat.message')).length;
  await sleep(4000);
  const m = (await events('chat.message')).length;
  check(m === n, `${m - n} messages after stop`);
  return 'ok';
});

await step('offline channel: friendly error', async () => {
  try {
    await js(`return api('live.start', { source: 'notarealchannel_xyz123', chat: false })`);
  } catch (e) {
    return 'refused: ' + e.message;
  }
  for (let i = 0; i < 100; i++) {
    const errs = await events('live.error');
    if (errs.length) return errs.at(-1).message.split('\n')[0];
    await sleep(200);
  }
  throw new Error('no error shown');
});

fs.writeFileSync(`${out}/captions.json`, JSON.stringify(caps, null, 1));
const passed = results.filter(r => r.ok).length;
console.log(`\n${passed}/${results.length} passed`);
fs.writeFileSync(`${out}/results.json`, JSON.stringify(results, null, 1));
main.ws.close();
process.exit(passed === results.length ? 0 : 1);
