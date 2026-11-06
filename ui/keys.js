// Musical keys: the engine numbers them 0-11 major from C, 12-23 minor from C.

const MAJ = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
const MIN = [5, 12, 7, 2, 9, 4, 11, 6, 1, 8, 3, 10];
const MAJ_NAMES = ['C', 'D♭', 'D', 'E♭', 'E', 'F', 'F♯', 'G', 'A♭', 'A', 'B♭', 'B'];
const MIN_NAMES = ['Cm', 'C♯m', 'Dm', 'E♭m', 'Em', 'Fm', 'F♯m', 'Gm', 'G♯m', 'Am', 'B♭m', 'Bm'];

let camelot = true;
export function useCamelot(on) { camelot = on; }

export function keyCode(k) {
  const minor = k >= 12;
  const num = minor ? MIN[k - 12] : MAJ[k];
  return { num, minor, code: `${num}${minor ? 'A' : 'B'}`, name: minor ? MIN_NAMES[k - 12] : MAJ_NAMES[k] };
}

export function keyColor(k) {
  const { num, minor } = keyCode(k);
  return `hsl(${(num * 30 + 40) % 360} 72% ${minor ? 60 : 70}%)`;
}

export function keyLabel(k) {
  if (k == null) return '—';
  const c = keyCode(k);
  return camelot ? c.code : c.name;
}

export function keyChip(k, far = false) {
  return `<b class="${far ? 'far' : ''}" style="background:${keyColor(k)}" title="${keyCode(k).name} · ${keyCode(k).code}">${keyLabel(k)}</b>`;
}

// Mixes well: same key, its relative major/minor, or one step round the wheel.
export function compatible(a, b) {
  if (a == null || b == null) return true;
  const x = keyCode(a), y = keyCode(b);
  if (x.num === y.num) return true;
  const step = Math.min((x.num - y.num + 12) % 12, (y.num - x.num + 12) % 12);
  return step === 1 && x.minor === y.minor;
}
