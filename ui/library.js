// The song browser: Liked Songs, playlists and search, with BPM and key
// columns. Long lists are drawn a screenful at a time.

import { keyChip, keyCode, compatible } from './keys.js';

const ROW = 38;

export class Library {
  constructor({ api, onLoad, onAdd, getContext }) {
    this.api = api;
    this.onLoad = onLoad;
    this.onAdd = onAdd;
    this.match = false;
    this.getContext = getContext; // { decks: [{id, key, bpm}], lead: {key, bpm} }
    this.rows = [];
    this.view = [];
    this.sel = 0;
    this.sort = null;
    this.source = 'liked';
    this.focus = 'tracks';
    this.sourceSel = 0;
    this.el = document.getElementById('rows');
    this.title = document.getElementById('list-title');
    this.count = document.getElementById('list-count');
    this.filter = document.getElementById('filter');
    this.spacer = document.createElement('div');
    this.spacer.style.position = 'relative';
    this.el.append(this.spacer);
    this.el.addEventListener('scroll', () => this.render());
    window.addEventListener('resize', () => this.render());
    this.filter.addEventListener('input', () => this.apply());
    document.querySelectorAll('.row.head .sort').forEach((h) => h.addEventListener('click', () => {
      const k = h.dataset.sort;
      this.sort = this.sort?.key === k ? (this.sort.dir === 1 ? { key: k, dir: -1 } : null) : { key: k, dir: 1 };
      document.querySelectorAll('.row.head .sort').forEach((x) => x.classList.toggle('by', this.sort?.key === x.dataset.sort));
      this.apply();
    }));
    const search = document.getElementById('search');
    let timer;
    search.addEventListener('input', () => {
      clearTimeout(timer);
      timer = setTimeout(() => { if (search.value.trim()) this.open('search', search.value.trim()); }, 350);
    });
    search.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && search.value.trim()) { clearTimeout(timer); this.open('search', search.value.trim()); search.blur(); }
      if (e.key === 'Escape') search.blur();
    });
    document.getElementById('sources').addEventListener('click', (e) => {
      const b = e.target.closest('.source');
      if (!b) return;
      this.open(b.dataset.src, b.dataset.id);
    });
    const matchBtn = document.getElementById('match');
    matchBtn.addEventListener('click', () => {
      this.match = !this.match;
      matchBtn.classList.toggle('on', this.match);
      if (this.match && !this.getContext().lead) window.toast('Load a song on a deck first: Match shows what mixes with it.');
      this.apply();
    });
    document.getElementById('queue-list').addEventListener('click', () => {
      const rows = this.view.filter((r) => r.playable);
      if (!rows.length) return;
      this.onAdd(rows);
    });
    document.getElementById('analyze-list').addEventListener('click', () => {
      const uris = this.view.filter((r) => r.playable && r.bpm == null).map((r) => r.uri);
      if (!uris.length) return window.toast('Every song here already has a BPM and key.');
      this.api.post('/api/analyze', { uris });
      window.toast(`Analyzing ${uris.length} song${uris.length === 1 ? '' : 's'} in the background…`);
    });
  }

  async loadPlaylists() {
    const box = document.getElementById('playlists');
    try {
      const { playlists } = await this.api.get('/api/playlists');
      box.innerHTML = '';
      for (const p of playlists) {
        if (p.folder !== undefined) {
          const f = document.createElement('div');
          f.className = 'folder';
          f.style.paddingLeft = `${10 + p.depth * 12}px`;
          f.textContent = '▸ ' + p.folder;
          box.append(f);
          continue;
        }
        const b = document.createElement('button');
        b.className = 'source';
        b.dataset.src = 'playlist';
        b.dataset.id = p.id;
        b.style.paddingLeft = `${10 + p.depth * 12}px`;
        b.textContent = p.name;
        b.title = `${p.name}${p.owner ? ' · ' + p.owner : ''} · ${p.len} songs`;
        box.append(b);
      }
      if (!playlists.length) box.innerHTML = '<div class="hint">No playlists yet</div>';
    } catch (e) {
      box.innerHTML = `<div class="hint">Couldn't load playlists</div>`;
    }
  }

  async open(src, id) {
    this.source = src;
    document.querySelectorAll('.source').forEach((b) => b.classList.toggle('active', b.dataset.src === src && (src !== 'playlist' || b.dataset.id === id)));
    this.title.textContent = src === 'search' ? `Searching “${id}”…` : 'Loading…';
    this.rows = [];
    this.view = [];
    this.render(true);
    const token = (this.token = Symbol());
    try {
      const url = src === 'liked' ? '/api/liked' : src === 'search' ? `/api/search?q=${encodeURIComponent(id)}` : `/api/playlist/${encodeURIComponent(id)}`;
      const data = await this.api.get(url);
      if (token !== this.token) return;
      this.title.textContent = data.title;
      this.rows = data.rows;
      this.sel = 0;
      this.el.scrollTop = 0;
      this.apply();
    } catch (e) {
      if (token !== this.token) return;
      this.title.textContent = 'Couldn’t load that';
      window.toast(e.message, true);
    }
  }

  apply() {
    const q = this.filter.value.trim().toLowerCase();
    let v = q ? this.rows.filter((r) => `${r.name} ${r.artist} ${r.album}`.toLowerCase().includes(q)) : this.rows.slice();
    if (this.match) {
      // Songs that mix well with the playing deck: key next to it on the
      // wheel, and a tempo within 6% (or half/double time).
      const ctx = this.getContext();
      this.matchSig = ctx.sig;
      const lead = ctx.lead;
      if (lead?.bpm) {
        const onDeck = new Set(ctx.decks.filter(Boolean).map((d) => d.id));
        const off = (bpm) => Math.min(...[0.5, 1, 2].map((m) => Math.abs((bpm * m) / lead.bpm - 1)));
        v = v
          .filter((r) => r.bpm && !onDeck.has(r.id) && off(r.bpm) < 0.06 && (lead.key == null || r.key == null || compatible(lead.key, r.key)))
          .sort((a, b) => off(a.bpm) - off(b.bpm));
      }
    }
    if (this.sort) {
      const { key, dir } = this.sort;
      const val = (r) => key === 'key' ? (r.key == null ? 999 : keyCode(r.key).num * 2 + (keyCode(r.key).minor ? 0 : 1)) : (r[key] ?? (typeof this.rows[0]?.[key] === 'string' ? '' : 1e9));
      v.sort((a, b) => {
        const x = val(a), y = val(b);
        return (typeof x === 'string' ? x.localeCompare(y) : x - y) * dir;
      });
    }
    this.view = v;
    this.sel = Math.min(this.sel, Math.max(0, v.length - 1));
    const analysed = v.filter((r) => r.bpm != null).length;
    this.count.textContent = `${v.length} song${v.length === 1 ? '' : 's'}${v.length ? ` · ${analysed} analyzed` : ''}`;
    this.render(true);
  }

  updateAnalysis(id, bpm, key) {
    let hit = false;
    for (const r of this.rows) if (r.id === id) { r.bpm = bpm; r.key = key; hit = true; }
    if (hit) this.apply();
  }

  render(force) {
    if (this.match && this.getContext().sig !== this.matchSig) {
      this.apply();
      return;
    }
    const top = this.el.scrollTop;
    const h = this.el.clientHeight;
    const first = Math.max(0, Math.floor(top / ROW) - 6);
    const last = Math.min(this.view.length, Math.ceil((top + h) / ROW) + 6);
    const ctx = this.getContext();
    const sig = `${first}:${last}:${this.sel}:${this.view.length}:${ctx.sig}`;
    if (!force && sig === this.sig) return;
    this.sig = sig;
    this.spacer.style.height = `${this.view.length * ROW}px`;
    if (!this.view.length) {
      this.spacer.innerHTML = this.title.textContent.endsWith('…') ? '<div class="loading-list">Loading…</div>' : '<div class="loading-list">Nothing here.</div>';
      return;
    }
    const html = [];
    for (let i = first; i < last; i++) {
      const r = this.view[i];
      const on = ctx.decks.map((d, n) => (d?.id === r.id ? ` on${n}` : '')).join('');
      const keyHtml = r.key == null ? '' : keyChip(r.key, ctx.lead?.key != null && !compatible(ctx.lead.key, r.key));
      const near = r.bpm && ctx.lead?.bpm && Math.abs(r.bpm / ctx.lead.bpm - 1) < 0.06 ? ' near' : '';
      const art = r.image ? ` style="background-image:url('${r.image}')"` : '';
      html.push(`<div class="row${i === this.sel ? ' sel' : ''}${r.playable ? '' : ' dead'}${on}" style="position:absolute;left:0;right:0;top:${i * ROW}px" data-i="${i}" draggable="${r.playable}">
        <span class="c-art"${art}></span>
        <span class="c-title" title="${esc(r.name)}">${esc(r.name)}${r.explicit ? ' <small style="color:var(--faint)">E</small>' : ''}</span>
        <span class="c-artist">${esc(r.artist)}</span>
        <span class="c-bpm${near}">${r.bpm ? r.bpm.toFixed(1) : ''}</span>
        <span class="c-key">${keyHtml}</span>
        <span class="c-time">${mmss(r.duration_ms / 1000)}</span>
        <span class="c-album">${esc(r.album)}</span>
        <span class="c-load">${r.playable ? '<button data-add="1" title="Add to Auto-DJ">+</button><button data-d="0" title="Load on deck 1">1</button><button data-d="1" title="Load on deck 2">2</button>' : ''}</span>
      </div>`);
    }
    this.spacer.innerHTML = html.join('');
  }

  bind() {
    this.spacer.addEventListener('click', (e) => {
      const row = e.target.closest('.row');
      if (!row) return;
      this.sel = +row.dataset.i;
      const btn = e.target.closest('.c-load button');
      if (btn?.dataset.add) this.onAdd([this.view[this.sel]]);
      else if (btn) this.onLoad(+btn.dataset.d, this.view[this.sel]);
      this.render(true);
    });
    this.spacer.addEventListener('dblclick', (e) => {
      const row = e.target.closest('.row');
      if (!row || e.target.closest('.c-load')) return;
      this.onLoad(null, this.view[+row.dataset.i]);
    });
    this.spacer.addEventListener('dragstart', (e) => {
      const row = e.target.closest('.row');
      if (!row) return;
      const r = this.view[+row.dataset.i];
      e.dataTransfer.setData('text/x-slipmat-uri', r.uri);
      e.dataTransfer.setData('text/plain', r.name);
      e.dataTransfer.effectAllowed = 'copy';
    });
  }

  move(delta) {
    if (this.focus === 'sources') {
      const list = [...document.querySelectorAll('#sources .source')];
      this.sourceSel = Math.max(0, Math.min(list.length - 1, this.sourceSel + delta));
      list.forEach((b, i) => b.style.outline = i === this.sourceSel ? '1px solid var(--text)' : '');
      list[this.sourceSel]?.scrollIntoView({ block: 'nearest' });
      return;
    }
    if (!this.view.length) return;
    this.sel = Math.max(0, Math.min(this.view.length - 1, this.sel + delta));
    const y = this.sel * ROW;
    if (y < this.el.scrollTop) this.el.scrollTop = y;
    else if (y + ROW > this.el.scrollTop + this.el.clientHeight) this.el.scrollTop = y + ROW - this.el.clientHeight;
    this.render(true);
  }

  // The browse knob's push: switch between the playlist list and the songs.
  press() {
    const list = [...document.querySelectorAll('#sources .source')];
    if (this.focus === 'tracks') {
      this.focus = 'sources';
      this.sourceSel = Math.max(0, list.findIndex((b) => b.classList.contains('active')));
      this.move(0);
    } else {
      list.forEach((b) => b.style.outline = '');
      const b = list[this.sourceSel];
      this.focus = 'tracks';
      if (b) this.open(b.dataset.src, b.dataset.id);
    }
  }

  selected() {
    return this.view[this.sel];
  }

  rowByUri(uri) {
    return this.rows.find((r) => r.uri === uri);
  }
}

export function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}

export function mmss(s) {
  if (!isFinite(s) || s < 0) s = 0;
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
}
