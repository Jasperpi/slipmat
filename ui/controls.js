// Knobs and faders that behave like hardware: drag to move, double-click to reset,
// and they ignore the engine's echo of a value while the user is holding them.

const now = () => performance.now();

function polar(cx, cy, r, deg) {
  const a = (deg - 90) * Math.PI / 180;
  return [cx + r * Math.cos(a), cy + r * Math.sin(a)];
}

function arc(cx, cy, r, from, to) {
  if (Math.abs(to - from) < 0.5) to = from + 0.5;
  const [x1, y1] = polar(cx, cy, r, from);
  const [x2, y2] = polar(cx, cy, r, to);
  const large = Math.abs(to - from) > 180 ? 1 : 0;
  const sweep = to > from ? 1 : 0;
  return `M${x1.toFixed(2)} ${y1.toFixed(2)} A${r} ${r} 0 ${large} ${sweep} ${x2.toFixed(2)} ${y2.toFixed(2)}`;
}

export function knob({ label, center = 0.5, bipolar = true, small = false, onChange, format }) {
  const el = document.createElement('div');
  el.className = 'knob' + (small ? ' mini' : '');
  el.innerHTML = `<svg viewBox="0 0 40 40" aria-hidden="true">
      <path class="arc-bg" d="${arc(20, 20, 16, -135, 135)}"/>
      <path class="arc"/>
      <circle class="cap" cx="20" cy="20" r="11"/>
      <line class="tick" x1="20" y1="20" x2="20" y2="11"/>
    </svg><span class="k-label">${label}</span>`;
  el.title = label;
  const arcEl = el.querySelector('.arc');
  const tick = el.querySelector('.tick');
  const labelEl = el.querySelector('.k-label');
  let value = center;
  let held = 0;
  const draw = (v) => {
    const deg = -135 + v * 270;
    const origin = bipolar ? -135 + center * 270 : -135;
    arcEl.setAttribute('d', arc(20, 20, 16, Math.min(origin, deg), Math.max(origin, deg)));
    const [x, y] = polar(20, 20, 9, deg);
    tick.setAttribute('x2', x.toFixed(2));
    tick.setAttribute('y2', y.toFixed(2));
    if (format) labelEl.textContent = format(v) ?? label;
  };
  const set = (v, fromUser) => {
    v = Math.min(1, Math.max(0, v));
    if (v === value && !fromUser) return;
    value = v;
    draw(v);
    if (fromUser) onChange?.(v);
  };
  let startY = 0, startV = 0;
  el.addEventListener('pointerdown', (e) => {
    el.setPointerCapture(e.pointerId);
    startY = e.clientY;
    startV = value;
    held = Infinity;
    e.preventDefault();
  });
  el.addEventListener('pointermove', (e) => {
    if (held !== Infinity) return;
    const fine = e.shiftKey ? 0.2 : 1;
    let v = startV + (startY - e.clientY) * 0.005 * fine;
    // A little detent at the centre, as on a mixer.
    if (bipolar && Math.abs(v - center) < 0.015) v = center;
    set(v, true);
  });
  const release = () => { held = now() + 400; };
  el.addEventListener('pointerup', release);
  el.addEventListener('pointercancel', release);
  el.addEventListener('dblclick', () => { set(center, true); held = now() + 400; });
  el.addEventListener('wheel', (e) => {
    e.preventDefault();
    set(value - Math.sign(e.deltaY) * 0.02, true);
    held = now() + 400;
  }, { passive: false });
  draw(value);
  return {
    el,
    get value() { return value; },
    // From the engine: ignored while the user has hold of it.
    update(v) { if (now() > held) set(v, false); },
    setLabel(t) { labelEl.textContent = t; },
  };
}

// Vertical or horizontal fader. `value` 0..1; vertical faders have 1 at the
// top unless `topIsZero` (the tempo fader: minus at the top, like Pioneer's).
export function fader({ horizontal = false, topIsZero = false, center = null, onChange, title = '' }) {
  const el = document.createElement('div');
  el.className = 'fader' + (horizontal ? ' h' : '');
  el.title = title;
  el.innerHTML = `<div class="track"></div>${center !== null ? '<div class="centre"></div>' : ''}<div class="thumb"></div>`;
  const thumb = el.querySelector('.thumb');
  let value = center ?? 0;
  let held = 0;
  const frac = (v) => horizontal ? v : (topIsZero ? v : 1 - v);
  const draw = () => {
    const f = frac(value);
    if (horizontal) thumb.style.left = `calc(10px + ${f} * (100% - 20px))`;
    else thumb.style.top = `calc(11px + ${f} * (100% - 22px))`;
  };
  const set = (v, fromUser) => {
    v = Math.min(1, Math.max(0, v));
    if (v === value && !fromUser) return;
    value = v;
    draw();
    if (fromUser) onChange?.(v);
  };
  let start = 0, startV = 0, span = 1;
  el.addEventListener('pointerdown', (e) => {
    el.setPointerCapture(e.pointerId);
    const r = el.getBoundingClientRect();
    span = horizontal ? r.width - 20 : r.height - 22;
    start = horizontal ? e.clientX : e.clientY;
    // Grabbing the track away from the thumb jumps there (not on the tempo fader).
    if (e.target !== thumb && !topIsZero) {
      const f = horizontal ? (e.clientX - r.left - 10) / span : (e.clientY - r.top - 11) / span;
      const v = horizontal ? f : 1 - f;
      set(v, true);
    }
    startV = value;
    held = Infinity;
    e.preventDefault();
  });
  el.addEventListener('pointermove', (e) => {
    if (held !== Infinity) return;
    const d = ((horizontal ? e.clientX : e.clientY) - start) / span * (e.shiftKey ? 0.2 : 1);
    let v = horizontal ? startV + d : (topIsZero ? startV + d : startV - d);
    if (center !== null && Math.abs(v - center) < 0.012) v = center;
    set(v, true);
  });
  const release = () => { held = now() + 400; };
  el.addEventListener('pointerup', release);
  el.addEventListener('pointercancel', release);
  el.addEventListener('dblclick', () => { if (center !== null) { set(center, true); held = now() + 400; } });
  draw();
  return {
    el,
    get value() { return value; },
    update(v) { if (now() > held) set(v, false); },
  };
}
