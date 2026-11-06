// Slipmat's decks, mixer and library. All sound happens in the engine; this
// page shows its state (about 30 times a second) and sends it commands.

import { knob, fader } from './controls.js';
import { WaveData, SRC, HOP, CUE_COLORS } from './wave.js';
import { Library, esc } from './library.js';
import { keyLabel, keyColor, useCamelot } from './keys.js';

const FRAMES_PER_TICK = SRC * (60 / (100 / 3)) / 720;
const FX_KINDS = ['echo', 'reverb', 'flanger', 'phaser', 'roll', 'trans', 'crush', 'wobble'];
const FX_BEATS = ['1/8', '1/4', '1/2', '3/4', '1', '2', '4', '8', '16'];
const LOOPS = [[0.25, '¼'], [0.5, '½'], [1, '1'], [2, '2'], [4, '4'], [8, '8'], [16, '16']];
const JUMPS = [1, 2, 4, 8, 16, 32];
const ZOOMS = [2, 3, 4, 6, 8, 12, 16, 24];
const STEMS = [['VOX', 'Vocals'], ['MEL', 'Melody'], ['BASS', 'Bass'], ['DRUM', 'Drums']];
const STEM_COLORS = ['#f472b6', '#facc15', '#60a5fa', '#fb923c'];

const $ = (s, el = document) => el.querySelector(s);
const S = { engine: null, decks: [null, null], extras: {}, settings: null, zoom: 4, login: null };

// ---- server ------------------------------------------------------------------

const api = {
  async get(url) {
    const r = await fetch(url);
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j.error || r.statusText);
    return j;
  },
  async post(url, body) {
    const r = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body ?? {}) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j.error || r.statusText);
    return j;
  },
};

let ws;
function connect() {
  ws = new WebSocket(`ws://${location.host}/api/ws`);
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.t === 'state') onState(msg);
    else onEvent(msg);
  };
  ws.onclose = () => {
    $('#pill-spotify').textContent = 'Slipmat stopped';
    $('#pill-spotify').className = 'pill bad';
    setTimeout(connect, 1000);
  };
}
function send(obj) {
  if (ws?.readyState === 1) ws.send(JSON.stringify(obj));
}
const ctl = (c, args = {}) => send({ c, ...args });

// ---- toasts -------------------------------------------------------------------

window.toast = (text, error = false) => {
  const t = document.createElement('div');
  t.className = 'toast' + (error ? ' error' : '');
  t.textContent = text;
  $('#toasts').append(t);
  setTimeout(() => t.remove(), error ? 5000 : 2600);
};

// ---- small helpers -------------------------------------------------------------

function setText(el, v) { if (el._t !== v) { el._t = v; el.textContent = v; } }
function cls(el, c, on) { if (el.classList.contains(c) !== !!on) el.classList.toggle(c, !!on); }
function clock(s, tenths = true) {
  if (!isFinite(s)) s = 0;
  const neg = s < 0;
  s = Math.abs(s);
  const m = Math.floor(s / 60);
  const sec = s % 60;
  const body = tenths ? `${m}:${sec.toFixed(1).padStart(4, '0')}` : `${m}:${String(Math.floor(sec)).padStart(2, '0')}`;
  return (neg ? '-' : '') + body;
}
function btn(label, title, extra = '') {
  const b = document.createElement('button');
  b.className = 'btn ' + extra;
  b.innerHTML = label;
  if (title) b.title = title;
  return b;
}
function hold(el, down, up) {
  el.addEventListener('pointerdown', (e) => { if (e.button === 0) { el.setPointerCapture(e.pointerId); down(e); } });
  el.addEventListener('pointerup', (e) => { if (e.button === 0) up(e); });
  el.addEventListener('pointercancel', up);
}

// ---- decks --------------------------------------------------------------------------

function buildDeck(d) {
  const root = $(`#deck-${d}`);
  root.innerHTML = `
    <div class="pitch-col"><span class="label">TEMPO</span></div>
    <div class="body">
      <div class="deck-head empty">
        <div class="art"><span class="num">${d + 1}</span></div>
        <div class="titles"><div class="title">Drop a song here</div><div class="artist">or highlight one and press LOAD ${d + 1}</div></div>
        <div class="bpm-block"><div class="bpm">—</div><div class="tempo">±0.00%</div></div>
      </div>
      <div class="overview"><canvas></canvas></div>
      <div class="deck-mid">
        <div class="platter"><div class="ring"></div><div class="spin"></div><div class="label"></div></div>
        <div class="readout">
          <div class="times"><span class="elapsed">0:00.0</span><span class="remain">-0:00.0</span></div>
          <div class="chips"><span class="chip key">—</span><span class="chip gain" title="Auto gain"></span><div class="beats"><i class="down"></i><i></i><i></i><i></i></div></div>
          <div class="stems"></div>
          <div class="grid-tools"></div>
        </div>
      </div>
      <div class="transport"></div>
      <div class="pads"></div>
      <div class="loops"></div>
    </div>`;
  const ui = {
    d, root,
    head: $('.deck-head', root), art: $('.art', root), title: $('.title', root), artist: $('.artist', root),
    bpm: $('.bpm', root), tempo: $('.tempo', root),
    overview: $('.overview canvas', root), platter: $('.platter', root), spin: $('.spin', root), label: $('.platter .label', root),
    elapsed: $('.elapsed', root), remain: $('.remain', root), key: $('.chip.key', root), gain: $('.chip.gain', root),
    beats: [...root.querySelectorAll('.beats i')],
    wave: null, est: { pos: 0, at: 0, speed: 0 }, ov: null, ovVersion: -1, ovW: 0, jump: 2,
  };

  // Tempo fader: minus at the top, like the controller.
  ui.pitch = fader({ topIsZero: true, center: 0.5, title: 'Tempo (double-click to reset)', onChange: (v) => ctl('pitch', { d, v: v * 2 - 1 }) });
  ui.pitch.el.style.setProperty('--c', `var(--d${d})`);
  $('.pitch-col', root).append(ui.pitch.el);
  const reset = btn('0', 'Back to the song’s own tempo', 'xs');
  reset.onclick = () => ctl('pitch_reset', { d });
  $('.pitch-col', root).append(reset);

  // Stems: click to drop one out, right-click (or shift-click) to solo it.
  const sb = $('.stems', root);
  ui.stemBtns = STEMS.map(([label, title], k) => {
    const b = btn(label, `${title} (click: on/off · right-click: solo)`, 'xs stem');
    b.style.setProperty('--sc', STEM_COLORS[k]);
    const press = (solo) => {
      const now = S.engine?.decks[d]?.stems ?? [1, 1, 1, 1];
      let gains;
      if (solo) {
        const only = now.map((_, i) => (i === k ? 1 : 0));
        gains = now.every((v, i) => v === only[i]) ? [1, 1, 1, 1] : only;
      } else {
        gains = now.slice();
        gains[k] = now[k] > 0.5 ? 0 : 1;
      }
      ctl('stems', { d, gains });
    };
    b.addEventListener('click', (e) => press(e.shiftKey));
    b.addEventListener('contextmenu', (e) => { e.preventDefault(); press(true); });
    sb.append(b);
    return b;
  });
  ui.stemNote = document.createElement('span');
  ui.stemNote.className = 'stem-note';
  sb.append(ui.stemNote);

  // Beat grid fixes.
  const gt = $('.grid-tools', root);
  const tools = [
    ['◀', 'Move the beat grid 5 ms earlier', () => ctl('grid_nudge', { d, ms: -5 })],
    ['▶', 'Move the beat grid 5 ms later', () => ctl('grid_nudge', { d, ms: 5 })],
    ['BEAT 1', 'Make the playhead the first beat of a bar', () => ctl('grid_downbeat', { d })],
    ['−', 'BPM −0.05', () => gridBpm(d, -0.05)],
    ['+', 'BPM +0.05', () => gridBpm(d, 0.05)],
    ['x2', 'Double the BPM', () => gridBpm(d, 'x2')],
    ['/2', 'Halve the BPM', () => gridBpm(d, '/2')],
  ];
  for (const [label, title, f] of tools) {
    const b = btn(label, title, 'xs');
    b.onclick = f;
    gt.append(b);
  }
  const gridLabel = document.createElement('span');
  gridLabel.className = 'chip';
  gridLabel.textContent = 'GRID';
  gridLabel.title = 'If the grid lines don’t sit on the kicks, fix them here';
  gt.prepend(gridLabel);

  // Transport.
  const tr = $('.transport', root);
  ui.cue = btn('CUE', 'Cue (hold to preview from the cue point)', 'big cue');
  hold(ui.cue, () => ctl('cue', { d, down: true }), () => ctl('cue', { d, down: false }));
  ui.play = btn('▶︎ ❚❚', 'Play / pause', 'big play');
  ui.play.onclick = () => ctl('play_pause', { d });
  ui.sync = btn('SYNC', 'Match tempo and beats with the other deck', 'sync');
  ui.sync.onclick = () => ctl('sync', { d, on: null });
  ui.keylock = btn('KEY', 'Key lock: changing tempo keeps the pitch');
  ui.keylock.onclick = () => ctl('key_lock', { d, on: !S.engine?.decks[d].keylock });
  ui.slip = btn('SLIP', 'Slip: after a scratch, loop or censor, carry on where the song would have been');
  ui.slip.onclick = () => ctl('slip', { d, on: !S.engine?.decks[d].slip });
  const nudgeDown = btn('−', 'Nudge slower (hold)', 'xs');
  const nudgeUp = btn('+', 'Nudge faster (hold)', 'xs');
  hold(nudgeDown, () => ctl('nudge', { d, v: -0.04 }), () => ctl('nudge', { d, v: 0 }));
  hold(nudgeUp, () => ctl('nudge', { d, v: 0.04 }), () => ctl('nudge', { d, v: 0 }));
  ui.range = document.createElement('select');
  ui.range.className = 'range';
  ui.range.title = 'Tempo range';
  ui.range.innerHTML = [0.06, 0.08, 0.1, 0.16, 0.5].map((r) => `<option value="${r}">±${Math.round(r * 100)}%</option>`).join('');
  ui.range.onchange = () => ctl('range', { d, range: +ui.range.value });
  const eject = btn('⏏', 'Unload this deck', 'xs');
  eject.onclick = () => ctl('eject', { d });
  const spacer = document.createElement('span');
  spacer.className = 'spacer';
  tr.append(ui.cue, ui.play, ui.sync, ui.keylock, ui.slip, spacer, nudgeDown, nudgeUp, ui.range, eject);

  // Hot cues.
  ui.pads = [];
  for (let n = 0; n < 8; n++) {
    const p = document.createElement('button');
    p.className = 'pad';
    p.style.setProperty('--pc', CUE_COLORS[n]);
    p.textContent = n + 1;
    p.title = `Hot cue ${n + 1} (right-click to delete)`;
    p.addEventListener('pointerdown', (e) => {
      if (e.button === 2 || e.shiftKey) return ctl('hotcue_clear', { d, n });
      p.setPointerCapture(e.pointerId);
      ctl('hotcue', { d, n, down: true });
    });
    p.addEventListener('pointerup', (e) => { if (e.button === 0 && !e.shiftKey) ctl('hotcue', { d, n, down: false }); });
    p.addEventListener('contextmenu', (e) => e.preventDefault());
    $('.pads', root).append(p);
    ui.pads.push(p);
  }

  // Loops and beat jumps.
  const lp = $('.loops', root);
  ui.loopBtns = LOOPS.map(([beats, label]) => {
    const b = btn(label, `${label}-beat loop`, 'xs');
    b.onclick = () => ctl('auto_loop', { d, beats });
    lp.append(b);
    return [beats, b];
  });
  const sep = () => { const s = document.createElement('span'); s.className = 'sep'; return s; };
  lp.append(sep());
  ui.loopIn = btn('IN', 'Loop in', 'xs');
  ui.loopIn.onclick = () => ctl('loop_in', { d });
  ui.loopOut = btn('OUT', 'Loop out', 'xs');
  ui.loopOut.onclick = () => ctl('loop_out', { d });
  ui.reloop = btn('⟲', 'Reloop / exit loop', 'xs');
  ui.reloop.onclick = () => ctl('reloop', { d });
  const half = btn('/2', 'Halve the loop', 'xs');
  half.onclick = () => ctl('loop_halve', { d });
  const dbl = btn('x2', 'Double the loop', 'xs');
  dbl.onclick = () => ctl('loop_double', { d });
  lp.append(ui.loopIn, ui.loopOut, ui.reloop, half, dbl, sep());
  const back = btn('◀', 'Jump back', 'xs');
  const size = btn(String(JUMPS[ui.jump]), 'Beat jump size (click to change)', 'xs');
  const fwd = btn('▶', 'Jump forward', 'xs');
  back.onclick = () => ctl('beat_jump', { d, beats: -JUMPS[ui.jump] });
  fwd.onclick = () => ctl('beat_jump', { d, beats: JUMPS[ui.jump] });
  size.onclick = () => { ui.jump = (ui.jump + 1) % JUMPS.length; size.textContent = JUMPS[ui.jump]; };
  lp.append(back, size, fwd);

  // Overview: click or drag to jump around the song.
  const seek = (e) => {
    const r = ui.overview.getBoundingClientRect();
    ctl('seek', { d, frac: Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) });
  };
  ui.overview.addEventListener('pointerdown', (e) => { ui.overview.setPointerCapture(e.pointerId); seek(e); });
  ui.overview.addEventListener('pointermove', (e) => { if (e.buttons) seek(e); });

  // Platter: grab it to scratch.
  let lastAngle = null;
  const angle = (e) => {
    const r = ui.platter.getBoundingClientRect();
    return Math.atan2(e.clientY - (r.top + r.height / 2), e.clientX - (r.left + r.width / 2)) * 180 / Math.PI;
  };
  ui.platter.addEventListener('pointerdown', (e) => {
    ui.platter.setPointerCapture(e.pointerId);
    lastAngle = angle(e);
    ctl('jog_touch', { d, on: true });
  });
  ui.platter.addEventListener('pointermove', (e) => {
    if (lastAngle === null) return;
    const a = angle(e);
    let delta = a - lastAngle;
    if (delta > 180) delta -= 360;
    if (delta < -180) delta += 360;
    lastAngle = a;
    if (delta) ctl('jog_scratch', { d, ticks: delta / 360 * 720 });
  });
  const letGo = () => { if (lastAngle !== null) { lastAngle = null; ctl('jog_touch', { d, on: false }); } };
  ui.platter.addEventListener('pointerup', letGo);
  ui.platter.addEventListener('pointercancel', letGo);

  dropTarget(root, () => d);
  return ui;
}

function gridBpm(d, by) {
  const g = S.engine?.decks[d]?.grid;
  if (!g) return;
  const bpm = by === 'x2' ? g.bpm * 2 : by === '/2' ? g.bpm / 2 : Math.round((g.bpm + by) * 100) / 100;
  ctl('grid_bpm', { d, bpm });
}

function dropTarget(el, deckAt) {
  el.addEventListener('dragover', (e) => {
    if (!e.dataTransfer.types.includes('text/x-slipmat-uri')) return;
    e.preventDefault();
    const d = deckAt(e);
    document.querySelectorAll('.deck').forEach((x) => cls(x, 'drop', +x.dataset.d === d));
  });
  el.addEventListener('dragleave', () => document.querySelectorAll('.deck').forEach((x) => cls(x, 'drop', false)));
  el.addEventListener('drop', (e) => {
    e.preventDefault();
    document.querySelectorAll('.deck').forEach((x) => cls(x, 'drop', false));
    const uri = e.dataTransfer.getData('text/x-slipmat-uri');
    if (uri) load(deckAt(e), { uri });
  });
}

function load(d, row) {
  if (!row?.uri) return;
  const st = S.engine?.decks;
  if (d === null) {
    d = st ? [0, 1].find((i) => !st[i].loaded) ?? [0, 1].find((i) => !st[i].playing) : 0;
    if (d === undefined) return toast('Both decks are playing. Pause one to load a song onto it.', true);
  }
  if (st?.[d]?.playing) return toast(`Deck ${d + 1} is playing. Pause it first.`, true);
  send({ t: 'load', d, uri: row.uri });
}

// ---- mixer ------------------------------------------------------------------------

function buildMixer() {
  const root = $('#mixer');
  const m = { strips: [] };
  const strips = [0, 1].map((d) => {
    const s = document.createElement('div');
    s.className = 'strip';
    s.dataset.d = d;
    const knobs = document.createElement('div');
    knobs.className = 'knobs';
    const trim = knob({ label: 'TRIM', small: true, onChange: (v) => ctl('trim', { d, v }) });
    const hi = knob({ label: 'HI', onChange: (v) => ctl('eq', { d, band: 2, v }) });
    const mid = knob({ label: 'MID', onChange: (v) => ctl('eq', { d, band: 1, v }) });
    const low = knob({ label: 'LOW', onChange: (v) => ctl('eq', { d, band: 0, v }) });
    const filter = knob({ label: 'FILTER', onChange: (v) => ctl('filter', { d, v: v * 2 - 1 }) });
    const cue = btn('CUE', 'Hear this channel in the headphones', 'cuebtn');
    cue.onclick = () => ctl('pfl', { d, on: !S.engine?.decks[d].pfl });
    // Two columns: EQ on the outside, trim, filter and cue inside.
    const order = d === 0 ? [hi.el, trim.el, mid.el, filter.el, low.el, cue] : [trim.el, hi.el, filter.el, mid.el, cue, low.el];
    knobs.append(...order);
    const lower = document.createElement('div');
    lower.className = 'lower';
    const vu = document.createElement('div');
    vu.className = 'vu';
    vu.innerHTML = '<i></i>';
    const f = fader({ title: 'Channel fader', onChange: (v) => ctl('fader', { d, v }) });
    f.el.style.setProperty('--c', `var(--d${d})`);
    if (d === 0) lower.append(vu, f.el); else lower.append(f.el, vu);
    s.append(knobs, lower);
    const strip = { trim, eq: [low, mid, hi], filter, cue, vu: vu.firstChild, fader: f };
    m.strips.push(strip);
    return s;
  });
  const centre = document.createElement('div');
  centre.className = 'centre-col';
  m.master = knob({ label: 'MASTER', bipolar: false, center: 0.8, onChange: (v) => ctl('master', { v }) });
  m.headMix = knob({ label: 'CUE/MST', bipolar: false, center: 0, small: true, onChange: (v) => ctl('head_mix', { v }) });
  m.headMix.el.title = 'Headphones: cued channels (left) to master (right)';
  m.headLevel = knob({ label: 'PHONES', bipolar: false, center: 0.8, small: true, onChange: (v) => ctl('head_level', { v }) });
  m.quantize = btn('Q', 'Quantize: hot cues, loops and jumps land on the beat', 'xs');
  m.quantize.onclick = () => ctl('quantize', { on: !S.engine?.quantize });
  m.phase = document.createElement('div');
  m.phase.className = 'phase';
  m.phase.title = 'Beat alignment between the decks';
  m.phase.innerHTML = '<i></i>';
  centre.append(m.master.el, m.headMix.el, m.headLevel.el, m.quantize, m.phase);
  root.append(strips[0], centre, strips[1]);

  // Crossfader with assign switches and curve.
  const xf = document.createElement('div');
  xf.className = 'xf';
  const assign = (d) => {
    const box = document.createElement('div');
    box.className = 'xf-assign';
    const bs = [[-1, 'A'], [0, 'THRU'], [1, 'B']].map(([side, label]) => {
      const b = btn(label, `Deck ${d + 1} on crossfader side ${label}`, 'xs');
      b.onclick = () => ctl('xf_assign', { d, side });
      box.append(b);
      return [side, b];
    });
    return [box, bs];
  };
  const [a0, b0] = assign(0);
  const [a1, b1] = assign(1);
  m.assign = [b0, b1];
  m.xf = fader({ horizontal: true, center: 0.5, title: 'Crossfader (double-click to centre)', onChange: (v) => ctl('crossfader', { v }) });
  m.curve = document.createElement('select');
  m.curve.className = 'curve';
  m.curve.title = 'Crossfader curve';
  m.curve.innerHTML = '<option value="smooth">Smooth</option><option value="linear">Linear</option><option value="sharp">Sharp</option>';
  m.curve.onchange = () => ctl('curve', { curve: m.curve.value });
  xf.append(a0, m.xf.el, a1);
  const foot = document.createElement('div');
  foot.className = 'xf-foot';
  const curveLabel = document.createElement('span');
  curveLabel.className = 'fx-name';
  curveLabel.textContent = 'CURVE';
  foot.append(curveLabel, m.curve);
  root.append(xf, foot);

  // Two effect units, up in the top bar.
  const fxRow = $('#fxbar');
  m.fx = [0, 1].map((u) => {
    const row = document.createElement('div');
    row.className = 'fxunit';
    const name = document.createElement('span');
    name.className = 'fx-name';
    name.textContent = `FX${u + 1}`;
    const sel = document.createElement('select');
    sel.innerHTML = FX_KINDS.map((k) => `<option value="${k}">${k[0].toUpperCase() + k.slice(1)}</option>`).join('');
    sel.onchange = () => ctl('fx_kind', { u, kind: sel.value });
    const less = btn('◀', 'Shorter', 'xs');
    const beats = document.createElement('span');
    beats.className = 'fx-beats';
    const more = btn('▶', 'Longer', 'xs');
    less.onclick = () => ctl('fx_beats', { u, i: Math.max(0, S.engine.fx[u].beats - 1) });
    more.onclick = () => ctl('fx_beats', { u, i: Math.min(FX_BEATS.length - 1, S.engine.fx[u].beats + 1) });
    const depth = knob({ label: 'DEPTH', bipolar: false, small: true, onChange: (v) => ctl('fx_depth', { u, v }) });
    depth.el.title = 'Depth (how much of the effect)';
    const on = btn('ON', 'Effect on/off', 'xs');
    on.onclick = () => ctl('fx_on', { u, on: !S.engine.fx[u].on });
    const targets = [[1, '1'], [2, '2'], [128, 'M']].map(([bit, label]) => {
      const b = btn(label, label === 'M' ? 'Apply to the master' : `Apply to deck ${label}`, 'xs');
      b.onclick = () => ctl('fx_assign', { u, mask: S.engine.fx[u].assign ^ bit });
      return [bit, b];
    });
    row.append(name, sel, less, beats, more, depth.el, on, ...targets.map((t) => t[1]));
    fxRow.append(row);
    return { row, sel, beats, depth, on, targets };
  });
  return m;
}

// ---- lanes (the big scrolling waveforms) -----------------------------------------

const lanes = $('#lane-canvas');
let laneDrag = null;
lanes.addEventListener('pointerdown', (e) => {
  const r = lanes.getBoundingClientRect();
  const d = e.clientY - r.top < r.height / 2 ? 0 : 1;
  if (!S.engine?.decks[d]?.loaded) return;
  lanes.setPointerCapture(e.pointerId);
  laneDrag = { d, x: e.clientX };
  ctl('jog_touch', { d, on: true });
});
lanes.addEventListener('pointermove', (e) => {
  if (!laneDrag) return;
  const dx = e.clientX - laneDrag.x;
  laneDrag.x = e.clientX;
  const r = lanes.getBoundingClientRect();
  const frames = -dx * (S.zoom * SRC) / r.width;
  if (frames) ctl('jog_scratch', { d: laneDrag.d, ticks: frames / FRAMES_PER_TICK });
});
const laneUp = () => { if (laneDrag) { ctl('jog_touch', { d: laneDrag.d, on: false }); laneDrag = null; } };
lanes.addEventListener('pointerup', laneUp);
lanes.addEventListener('pointercancel', laneUp);
lanes.addEventListener('wheel', (e) => { e.preventDefault(); zoom(e.deltaY > 0 ? 1 : -1); }, { passive: false });
dropTarget($('#lanes'), (e) => {
  const r = lanes.getBoundingClientRect();
  return e.clientY - r.top < r.height / 2 ? 0 : 1;
});
function zoom(dir) {
  const i = ZOOMS.indexOf(S.zoom);
  S.zoom = ZOOMS[Math.max(0, Math.min(ZOOMS.length - 1, (i < 0 ? 3 : i) + dir))];
}
$('#zoom-in').onclick = () => zoom(-1);
$('#zoom-out').onclick = () => zoom(1);

function fitCanvas(c) {
  const dpr = window.devicePixelRatio || 1;
  const w = Math.round(c.clientWidth * dpr), h = Math.round(c.clientHeight * dpr);
  if (c.width !== w || c.height !== h) { c.width = w; c.height = h; }
  return [w, h, dpr];
}

function drawLanes(t) {
  const [W, H, dpr] = fitCanvas(lanes);
  const ctx = lanes.getContext('2d');
  ctx.fillStyle = '#08090a';
  ctx.fillRect(0, 0, W, H);
  const laneH = H / 2;
  const span = S.zoom * SRC; // frames across the width
  for (const ui of decks) {
    const d = ui.d;
    const y0 = d * laneH;
    const st = S.engine?.decks[d];
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, y0, W, laneH);
    ctx.clip();
    if (st?.loaded && ui.wave) {
      const pos = livePos(ui, t);
      const left = pos - span / 2;
      const px = (f) => (f - left) / span * W;
      ui.wave.paint(Math.round(laneH - 8 * dpr));
      ui.wave.blit(ctx, left / HOP, span / HOP, 0, y0 + 4 * dpr, W, laneH - 8 * dpr);
      // Not downloaded yet.
      if (!st.done) {
        const x = px(st.decoded);
        if (x < W) {
          ctx.fillStyle = 'rgba(255,255,255,0.04)';
          ctx.fillRect(Math.max(0, x), y0, W, laneH);
        }
      }
      // Loop region.
      const lp = st.loop;
      if (lp) {
        ctx.fillStyle = lp.active ? 'rgba(52,211,153,0.18)' : 'rgba(52,211,153,0.07)';
        ctx.fillRect(px(lp.start), y0, (lp.end - lp.start) / span * W, laneH);
        ctx.fillStyle = '#34d399';
        ctx.fillRect(px(lp.start) - dpr, y0, 2 * dpr, laneH);
        ctx.fillRect(px(lp.end) - dpr, y0, 2 * dpr, laneH);
      }
      // Beat grid.
      const g = st.grid;
      if (g) {
        const period = SRC * 60 / g.bpm;
        const k0 = Math.ceil((left - g.anchor) / period);
        for (let k = k0; ; k++) {
          const f = g.anchor + k * period;
          if (f > left + span) break;
          const x = Math.round(px(f));
          const bar = ((k % 4) + 4) % 4 === 0;
          const phrase = bar && (((k / 4) % 4) + 4) % 4 === 0;
          // A faint line through the waveform, solid ticks at the edges.
          ctx.fillStyle = bar ? 'rgba(255,255,255,0.32)' : 'rgba(255,255,255,0.1)';
          ctx.fillRect(x, y0, dpr, laneH);
          ctx.fillStyle = phrase ? '#ff4d4d' : bar ? '#ffffff' : 'rgba(255,255,255,0.7)';
          const tick = (bar ? 9 : 5) * dpr;
          const w = (bar ? 2 : 1) * dpr;
          ctx.fillRect(x - (w > dpr ? dpr / 2 : 0), y0, w, tick);
          ctx.fillRect(x - (w > dpr ? dpr / 2 : 0), y0 + laneH - tick, w, tick);
        }
      }
      // Cue and hot cues.
      const flag = (f, color, label) => {
        const x = px(f);
        if (x < -20 * dpr || x > W + 20 * dpr) return;
        ctx.fillStyle = color;
        ctx.fillRect(x - dpr, y0, 2 * dpr, laneH);
        if (label) {
          ctx.fillRect(x, y0, 14 * dpr, 13 * dpr);
          ctx.fillStyle = '#0c0d0f';
          ctx.font = `700 ${9 * dpr}px "JetBrains Mono", monospace`;
          ctx.fillText(label, x + 3.5 * dpr, y0 + 10 * dpr);
        }
      };
      st.hotcues.forEach((c, n) => c != null && flag(c, CUE_COLORS[n], String(n + 1)));
      ctx.fillStyle = '#ffffff';
      const cx = px(st.cue);
      ctx.beginPath();
      ctx.moveTo(cx - 6 * dpr, y0 + laneH);
      ctx.lineTo(cx + 6 * dpr, y0 + laneH);
      ctx.lineTo(cx, y0 + laneH - 8 * dpr);
      ctx.fill();
    } else {
      ctx.fillStyle = '#3a3c43';
      ctx.font = `500 ${12 * dpr}px Inter, sans-serif`;
      ctx.fillText(st?.loaded ? 'Loading…' : `Deck ${d + 1} is empty — drag a song here`, 16 * dpr, y0 + laneH / 2 + 4 * dpr);
    }
    // Deck tab.
    ctx.fillStyle = d === 0 ? '#ff6a3d' : '#3cb6ff';
    ctx.fillRect(0, y0, 4 * dpr, laneH);
    ctx.restore();
  }
  // Playhead and separator.
  ctx.fillStyle = 'rgba(255,255,255,0.9)';
  ctx.fillRect(Math.round(W / 2) - dpr, 0, 2 * dpr, H);
  ctx.fillStyle = '#2a2b31';
  ctx.fillRect(0, laneH - dpr / 2, W, dpr);
}

function drawOverview(ui, t) {
  const c = ui.overview;
  const [W, H, dpr] = fitCanvas(c);
  const ctx = c.getContext('2d');
  ctx.fillStyle = '#0a0b0c';
  ctx.fillRect(0, 0, W, H);
  const st = S.engine?.decks[ui.d];
  if (!st?.loaded || !ui.wave) return;
  const len = Math.max(st.len, 1);
  ui.wave.expected = len / HOP;
  if (ui.ovVersion !== ui.wave.version || ui.ovW !== W) {
    if (!ui.ovPending) {
      ui.ovPending = true;
      requestAnimationFrame(() => {
        ui.ov = ui.wave.overview(W, H);
        ui.ovVersion = ui.wave.version;
        ui.ovW = W;
        ui.ovPending = false;
      });
    }
  }
  if (ui.ov) ctx.drawImage(ui.ov, 0, 0);
  const pos = livePos(ui, t);
  const x = pos / len * W;
  ctx.fillStyle = 'rgba(8,9,10,0.55)';
  ctx.fillRect(0, 0, x, H);
  if (!st.done) {
    ctx.fillStyle = 'rgba(255,255,255,0.06)';
    ctx.fillRect(st.decoded / len * W, 0, W, H);
  }
  if (st.loop) {
    ctx.fillStyle = 'rgba(52,211,153,0.35)';
    ctx.fillRect(st.loop.start / len * W, 0, Math.max(2, (st.loop.end - st.loop.start) / len * W), H);
  }
  st.hotcues.forEach((cue, n) => {
    if (cue == null) return;
    ctx.fillStyle = CUE_COLORS[n];
    ctx.fillRect(cue / len * W - dpr / 2, 0, 1.5 * dpr, H);
    ctx.fillRect(cue / len * W - dpr / 2, 0, 5 * dpr, 5 * dpr);
  });
  ctx.fillStyle = '#fff';
  ctx.fillRect(st.cue / len * W - dpr / 2, H - 5 * dpr, 4 * dpr, 5 * dpr);
  ctx.fillStyle = '#fff';
  ctx.fillRect(x - dpr, 0, 2 * dpr, H);
  // Minute ticks along the bottom.
  ctx.fillStyle = 'rgba(255,255,255,0.15)';
  for (let m = 60 * SRC; m < len; m += 60 * SRC) ctx.fillRect(m / len * W, H - 4 * dpr, dpr, 4 * dpr);
}

// Where the deck is right now, between state messages.
function livePos(ui, t) {
  const st = S.engine?.decks[ui.d];
  if (!st) return 0;
  const e = ui.est;
  const p = e.pos + e.speed * SRC * (t - e.at) / 1000;
  return Math.max(0, Math.min(st.len || p, p));
}

// ---- state -> page -------------------------------------------------------------

function trackWave(ui, meta) {
  const key = meta?.track_key ?? 0;
  if (!meta) { ui.wave = null; ui.ov = null; return; }
  if (ui.wave?.key === key) return;
  const wave = new WaveData(key);
  ui.wave = wave;
  ui.ov = null;
  ui.ovVersion = -1;
  const pull = async () => {
    if (ui.wave !== wave) return;
    try {
      const r = await fetch(`/api/deck/${ui.d}/wave?from=${wave.cols}`);
      if (r.ok && r.headers.get('x-track-key') === String(key)) {
        wave.append(new Uint8Array(await r.arrayBuffer()));
        wave.done = r.headers.get('x-wave-done') === '1' && wave.cols >= +r.headers.get('x-wave-total');
      }
    } catch {}
    if (!wave.done) setTimeout(pull, 250);
  };
  pull();
}

function leadDeck() {
  const st = S.engine?.decks;
  if (!st) return null;
  const playing = [0, 1].filter((d) => st[d].playing && S.decks[d]);
  const pick = playing.sort((a, b) => st[b].fader - st[a].fader)[0] ?? [0, 1].find((d) => S.decks[d]);
  if (pick === undefined) return null;
  const meta = S.decks[pick];
  const g = st[pick].grid;
  return { key: meta.key, bpm: g ? g.bpm * st[pick].tempo : meta.bpm };
}

function onState(msg) {
  const t = performance.now();
  const first = !S.engine;
  S.engine = msg.engine;
  S.decks = msg.decks;
  S.extras = msg.extras || S.extras;
  const e = S.engine;
  for (const ui of decks) {
    const d = ui.d;
    const st = e.decks[d];
    const meta = S.decks[d];
    if (!st) continue;
    // Follow the engine's clock, smoothing out the delivery jitter.
    const est = ui.est;
    const predicted = est.pos + est.speed * SRC * (t - est.at) / 1000;
    if (first || Math.abs(predicted - st.pos) > 2500 || !st.playing || st.touched || st.speed === 0) est.pos = st.pos;
    else est.pos = predicted + (st.pos - predicted) * 0.25;
    est.at = t;
    est.speed = st.speed;
    trackWave(ui, meta);
    cls(ui.head, 'empty', !meta);
    setText(ui.title, meta ? meta.name : 'Drop a song here');
    setText(ui.artist, meta ? meta.artist : `or highlight one and press LOAD ${d + 1}`);
    const img = meta?.image ? `url('${meta.image}')` : '';
    if (ui.art._img !== img) { ui.art._img = img; ui.art.style.backgroundImage = img; ui.label.style.backgroundImage = img; }
    const bpm = st.grid ? st.grid.bpm * st.tempo : null;
    ui.bpm.innerHTML = bpm ? `${bpm.toFixed(1)}<small>BPM</small>` : (meta?.analysing ? '<small>analyzing…</small>' : '—');
    const pct = (st.tempo - 1) * 100;
    setText(ui.tempo, `${pct >= 0 ? '+' : '−'}${Math.abs(pct).toFixed(2)}%${st.keylock ? ' · key lock' : ''}`);
    cls(ui.tempo, 'synced', st.sync);
    if (meta?.key != null) {
      setText(ui.key, keyLabel(meta.key));
      ui.key.classList.add('key');
      ui.key.style.background = keyColor(meta.key);
    } else {
      setText(ui.key, meta?.analysing ? 'key…' : '—');
      ui.key.classList.remove('key');
      ui.key.style.background = '';
    }
    setText(ui.gain, st.loaded && S.engine.auto_gain ? `${st.auto_db >= 0 ? '+' : ''}${st.auto_db.toFixed(1)} dB` : '');
    ui.gain.hidden = !st.loaded || !S.engine.auto_gain;
    ui.pitch.update((st.pitch + 1) / 2);
    if (document.activeElement !== ui.range) {
      const opt = [...ui.range.options].reduce((a, o) => Math.abs(+o.value - st.range) < Math.abs(+a.value - st.range) ? o : a);
      if (ui.range.value !== opt.value) ui.range.value = opt.value;
    }
    cls(ui.play, 'on', st.playing);
    cls(ui.play, 'blink', st.loaded && !st.playing);
    cls(ui.cue, 'on', st.loaded && st.at_cue);
    cls(ui.sync, 'on', st.sync);
    cls(ui.keylock, 'on', st.keylock);
    cls(ui.slip, 'on', st.slip);
    cls(ui.platter, 'touched', st.touched);
    ui.pads.forEach((p, n) => cls(p, 'set', st.hotcues[n] != null));
    // Stems.
    ui.stemBtns.forEach((b, k) => { cls(b, 'muted', st.stems[k] < 0.5); b.disabled = !st.stems_status; });
    const ss = st.stems_status;
    const pctDone = st.len ? Math.min(100, Math.floor(st.stems_ready / st.len * 100)) : 0;
    setText(ui.stemNote, !st.loaded ? '' : !ss ? 'stems off' : ss === 'done' ? '' : ss === 'downloading' ? 'getting stem models…' : ss === 'failed' ? 'stems failed' : ss === 'separating' ? `splitting ${pctDone}%` : ss === 'waiting' ? 'stems queued' : '');
    cls(ui.stemNote, 'bad', ss === 'failed');
    const lp = st.loop;
    const period = st.grid ? SRC * 60 / st.grid.bpm : 0;
    ui.loopBtns.forEach(([beats, b]) => cls(b, 'loopon', lp?.active && period && Math.abs(lp.end - lp.start - beats * period) < 2));
    cls(ui.loopIn, 'loopon', st.loop_in != null || lp?.active);
    cls(ui.loopOut, 'loopon', lp?.active);
    cls(ui.reloop, 'loopon', lp?.active);
    // Mixer strip.
    const m = mixer.strips[d];
    m.trim.update(st.trim);
    st.eq.forEach((v, b) => { m.eq[b].update(v); cls(m.eq[b].el, 'killed', v < 0.01); });
    m.filter.update((st.filter + 1) / 2);
    m.fader.update(st.fader);
    cls(m.cue, 'on', st.pfl);
    m.cue.style.setProperty('--c', `var(--d${d})`);
    mixer.assign[d].forEach(([side, b]) => cls(b, 'on', st.side === side));
  }
  mixer.master.update(e.master);
  mixer.headMix.update(e.head_mix);
  mixer.headLevel.update(e.head_level);
  mixer.xf.update(e.crossfader);
  cls(mixer.quantize, 'on', e.quantize);
  if (document.activeElement !== mixer.curve) mixer.curve.value = e.curve;
  e.fx.forEach((fx, u) => {
    const f = mixer.fx[u];
    if (document.activeElement !== f.sel) f.sel.value = fx.kind;
    setText(f.beats, FX_BEATS[fx.beats]);
    f.depth.update(fx.depth);
    cls(f.on, 'on', fx.on);
    cls(f.row, 'live', fx.on);
    f.targets.forEach(([bit, b]) => cls(b, 'on', fx.assign & bit));
  });
  pills();
  library.render();
}

function pills() {
  const x = S.extras;
  const login = x.login;
  const sp = $('#pill-spotify');
  if (login) {
    const map = {
      online: ['good', 'Spotify'],
      connecting: ['', 'Connecting to Spotify…'],
      reconnecting: ['warn', 'Reconnecting to Spotify…'],
      browser: ['warn', 'Finish signing in in your browser'],
      failed: ['bad', 'Spotify: ' + (login.message || 'failed')],
      signed_out: ['bad', 'Not signed in'],
    };
    const [k, text] = map[login.state] || ['', login.state];
    setText(sp, text);
    sp.className = 'pill ' + k;
    sp.title = login.state === 'online' ? `Signed in as ${login.user}` : text;
    loginOverlay(login);
  }
  const out = x.output;
  const po = $('#pill-output');
  if (out) {
    if (out.silent) {
      setText(po, 'No sound card');
      po.title = out.error || 'Nothing to play through';
      po.className = 'pill bad';
    } else {
      setText(po, `🔊 ${out.device}${out.headphones ? '' : ' · no cue out'}`);
      po.className = 'pill ' + (out.headphones ? 'good' : 'warn');
      po.title = `${out.device}: ${out.channels} channels, ${(out.rate / 1000).toFixed(1)} kHz, ~${out.latency_ms.toFixed(0)} ms.` + (out.headphones ? ' Master and headphones are routed.' : ' No separate headphone output: plug in the controller, or use split cue in Settings.');
    }
  }
  const midi = x.midi;
  const pc = $('#pill-controller');
  if (midi) {
    setText(pc, midi.connected ? `🎛 ${midi.connected}` : 'No controller');
    pc.className = 'pill ' + (midi.connected ? 'good' : '');
    pc.title = midi.connected ? `Last message: ${midi.last || '—'}` : `MIDI ports: ${(midi.ports || []).join(', ') || 'none'}`;
  }
  const b = x.batch;
  const pb = $('#pill-batch');
  pb.hidden = !b?.queued;
  if (b?.queued) setText(pb, `Analyzing ${b.done + 1}/${b.queued}${b.current ? ' · ' + b.current : ''}`);
}

let wasOnline = false;
function loginOverlay(login) {
  // Not for the second of "connecting" at every start; only when it needs the user.
  const show = ['signed_out', 'browser', 'failed'].includes(login.state) || (login.state === 'connecting' && !$('#login').hidden);
  $('#login').hidden = !show;
  $('#login-riff').hidden = !(login.state === 'signed_out' && login.riff);
  const status = { connecting: 'Connecting…', browser: 'Waiting for you to approve in the Spotify tab…', failed: login.message, signed_out: '' }[login.state] ?? '';
  setText($('#login-status'), status || '');
  if (login.state === 'online' && !wasOnline) {
    wasOnline = true;
    library.loadPlaylists();
    library.open('liked');
  }
  if (login.state === 'signed_out') wasOnline = false;
}

function onEvent(ev) {
  switch (ev.t) {
    case 'notice': toast(ev.text, ev.error); break;
    case 'browse': library.move(ev.delta); break;
    case 'browse_press': library.press(); break;
    case 'load_selected': load(ev.d, library.selected()); break;
    case 'analysed': library.updateAnalysis(ev.id, ev.bpm, ev.key); break;
  }
}

// ---- animation ------------------------------------------------------------------------

function frame(t) {
  requestAnimationFrame(frame);
  if (!S.engine) return;
  drawLanes(t);
  const e = S.engine;
  for (const ui of decks) {
    const st = e.decks[ui.d];
    if (!st) continue;
    drawOverview(ui, t);
    const pos = livePos(ui, t);
    const secs = pos / SRC;
    setText(ui.elapsed, clock(secs));
    const left = (st.len - pos) / SRC;
    setText(ui.remain, st.loaded ? '-' + clock(Math.max(0, left)) : '-0:00.0');
    cls(ui.remain, 'ending', st.playing && left < 30);
    // 33⅓ rpm.
    ui.spin.style.transform = `rotate(${(secs * 200) % 360}deg)`;
    const g = st.grid;
    const beat = g ? Math.floor((pos - g.anchor) / (SRC * 60 / g.bpm)) : null;
    ui.beats.forEach((b, i) => cls(b, 'on', beat !== null && ((beat % 4) + 4) % 4 === i));
    const m = mixer.strips[ui.d];
    const level = Math.max(st.meter[0], st.meter[1]);
    const db = 20 * Math.log10(Math.max(level, 1e-6));
    m.vu.style.height = `${Math.max(0, Math.min(1, (db + 48) / 48)) * 100}%`;
    m.vu.style.setProperty('--h', `${m.vu.parentElement.clientHeight}px`);
  }
  // Beat alignment meter.
  const [a, b] = e.decks;
  const pi = mixer.phase.firstChild;
  if (a?.grid && b?.grid && a.loaded && b.loaded) {
    const ph = (ui, st) => { const p = SRC * 60 / st.grid.bpm; return ((((livePos(ui, t) - st.grid.anchor) / p) % 1) + 1) % 1; };
    let diff = ph(decks[1], b) - ph(decks[0], a);
    if (diff > 0.5) diff -= 1;
    if (diff < -0.5) diff += 1;
    pi.style.left = `${50 + diff * 100}%`;
    pi.style.opacity = a.playing && b.playing ? 1 : 0.35;
    pi.style.background = Math.abs(diff) < 0.02 ? 'var(--ok)' : 'var(--sync)';
  } else {
    pi.style.opacity = 0;
  }
  const mm = e.master_meter;
  ['#mm-l', '#mm-r'].forEach((sel, i) => {
    const db = 20 * Math.log10(Math.max(mm[i], 1e-6));
    $(sel).style.clipPath = `inset(0 ${100 - Math.max(0, Math.min(1, (db + 48) / 48)) * 100}% 0 0)`;
  });
}

// ---- settings, login, help ----------------------------------------------------------

async function openSettings() {
  const s = await api.get('/api/settings');
  S.settings = s;
  const { devices } = await api.get('/api/devices');
  const dev = $('#set-device');
  dev.innerHTML = `<option value="">Automatic (the controller if plugged in)</option>` + devices.map((d) => `<option value="${esc(d.name)}">${esc(d.name)} · ${d.channels} ch${d.controller ? ' · controller' : ''}</option>`).join('');
  dev.value = s.output.device;
  $('#set-master').value = s.output.master.join(',');
  $('#set-cue').value = s.output.split_cue ? 'split' : s.output.cue ? s.output.cue.join(',') : 'none';
  $('#set-buffer').value = String(s.output.buffer);
  $('#set-keylock').checked = s.keylock;
  $('#set-quantize').checked = s.quantize;
  $('#set-autogain').checked = s.auto_gain;
  $('#set-vinyl').checked = s.vinyl;
  $('#set-invert').checked = s.invert_tempo;
  $('#set-range').value = String(s.range);
  $('#set-camelot').checked = s.camelot;
  $('#set-stems').checked = s.stems;
  const login = S.extras.login;
  setText($('#set-account'), login?.state === 'online' ? `Signed in as ${login.user}` : 'Not signed in');
  outputStatus(S.extras.output);
  $('#settings').hidden = false;
}

function outputStatus(out) {
  if (!out) return;
  $('#set-output-status').textContent = out.silent
    ? `No sound card is playing${out.error ? `: ${out.error}` : ''}.`
    : `Playing through ${out.device} — ${out.channels} channels at ${out.rate} Hz, about ${out.latency_ms.toFixed(0)} ms.`;
}

async function saveSettings() {
  const s = S.settings;
  if (!s) return;
  const cue = $('#set-cue').value;
  s.output = {
    device: $('#set-device').value,
    master: $('#set-master').value.split(',').map(Number),
    cue: cue === 'split' || cue === 'none' ? null : cue.split(',').map(Number),
    split_cue: cue === 'split',
    buffer: +$('#set-buffer').value,
  };
  s.keylock = $('#set-keylock').checked;
  s.quantize = $('#set-quantize').checked;
  s.auto_gain = $('#set-autogain').checked;
  s.vinyl = $('#set-vinyl').checked;
  s.invert_tempo = $('#set-invert').checked;
  s.range = +$('#set-range').value;
  s.camelot = $('#set-camelot').checked;
  s.stems = $('#set-stems').checked;
  useCamelot(s.camelot);
  try {
    const r = await api.post('/api/settings', s);
    outputStatus(r.output);
    library.render(true);
  } catch (e) {
    toast(e.message, true);
  }
}

document.querySelectorAll('#settings select, #settings input').forEach((el) => el.addEventListener('change', saveSettings));
$('#open-settings').onclick = openSettings;
$('#open-help').onclick = () => { $('#help').hidden = false; };
document.querySelectorAll('[data-close]').forEach((b) => b.onclick = () => { b.closest('.overlay').hidden = true; });
document.querySelectorAll('.overlay').forEach((o) => o.addEventListener('click', (e) => { if (e.target === o && o.id !== 'login') o.hidden = true; }));
$('#login-browser').onclick = () => api.post('/api/login/browser').catch((e) => toast(e.message, true));
$('#login-riff').onclick = () => api.post('/api/login/riff').catch((e) => toast(e.message, true));
$('#logout').onclick = async () => { await api.post('/api/logout'); $('#settings').hidden = true; };
$('#quit').onclick = async () => {
  if (S.engine?.decks.some((d) => d.playing)) return toast('A deck is still playing. Pause it before quitting.', true);
  await api.post('/api/quit').catch(() => {});
  document.body.innerHTML = '<div style="display:grid;place-items:center;height:100vh;color:#9a9ca4">Slipmat has quit. Start it again from your app launcher.</div>';
};

// ---- keyboard ------------------------------------------------------------------------------

const held = new Set();
document.addEventListener('keydown', (e) => {
  if (e.target.matches('input, select, textarea')) return;
  if (e.repeat && !['ArrowUp', 'ArrowDown'].includes(e.key)) return;
  const k = e.key.toLowerCase();
  if (e.key === 'ArrowDown') { library.move(1); e.preventDefault(); }
  else if (e.key === 'ArrowUp') { library.move(-1); e.preventDefault(); }
  else if (e.shiftKey && e.key === 'ArrowLeft') load(0, library.selected());
  else if (e.shiftKey && e.key === 'ArrowRight') load(1, library.selected());
  else if (e.key === '/') { $('#search').focus(); e.preventDefault(); }
  else if (e.key === 'Enter') load(null, library.selected());
  else if (k === 'z') { held.add('z'); ctl('cue', { d: 0, down: true }); }
  else if (k === 'x') ctl('play_pause', { d: 0 });
  else if (k === ',') { held.add(','); ctl('cue', { d: 1, down: true }); }
  else if (k === '.') ctl('play_pause', { d: 1 });
  else if ('1234'.includes(k) && k) ctl('hotcue', { d: 0, n: +k - 1, down: true });
  else if ('7890'.includes(k) && k) ctl('hotcue', { d: 1, n: '7890'.indexOf(k), down: true });
  else if (e.key === '?') $('#help').hidden = false;
  else if (e.key === 'Escape') document.querySelectorAll('.overlay:not(#login)').forEach((o) => o.hidden = true);
});
document.addEventListener('keyup', (e) => {
  const k = e.key.toLowerCase();
  if (k === 'z' && held.delete('z')) ctl('cue', { d: 0, down: false });
  else if (k === ',' && held.delete(',')) ctl('cue', { d: 1, down: false });
  else if ('1234'.includes(k) && k) ctl('hotcue', { d: 0, n: +k - 1, down: false });
  else if ('7890'.includes(k) && k) ctl('hotcue', { d: 1, n: '7890'.indexOf(k), down: false });
});

setInterval(() => {
  const d = new Date();
  setText($('#clock'), `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`);
}, 1000);

// ---- start -----------------------------------------------------------------------------------

const decks = [buildDeck(0), buildDeck(1)];
const mixer = buildMixer();
const library = new Library({
  api,
  onLoad: load,
  getContext: () => {
    const lead = leadDeck();
    return { decks: S.decks.map((m) => m && { id: m.id }), lead, sig: `${S.decks.map((m) => m?.id).join()}|${lead?.key}|${lead?.bpm?.toFixed(1)}` };
  },
});
library.bind();
api.get('/api/settings').then((s) => { S.settings = s; useCamelot(s.camelot); }).catch(() => {});
connect();
requestAnimationFrame(frame);
