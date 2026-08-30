'use strict';

// Newest lines go at the BOTTOM and the view follows them, like `tail -f`.
// (The previous UI put backfill newest-first at the top but inserted live lines
// at the bottom, so the two halves disagreed about ordering.)

const LEVELS = ['trace', 'debug', 'info', 'warn', 'error'];
/** Hard cap on retained lines. Without it a long-lived tab grows until it dies. */
const MAX_BUFFER = 5000;
/** A message longer than this, or containing newlines/JSON, gets a expander. */
const COLLAPSE_OVER = 180;

const el = {
  logs: document.getElementById('logs'),
  search: document.getElementById('search'),
  level: document.getElementById('level'),
  device: document.getElementById('device'),
  pause: document.getElementById('pause'),
  follow: document.getElementById('follow'),
  clear: document.getElementById('clear'),
  copy: document.getElementById('copy'),
  dot: document.getElementById('dot'),
  conn: document.getElementById('conn'),
  count: document.getElementById('count'),
  shown: document.getElementById('shown'),
  logout: document.getElementById('logout'),
};

const state = {
  logs: [],
  paused: false,
  follow: true,
  search: '',
  minLevel: 0,
  deviceId: '',
  lastId: 0,
  source: null,
};

/* ---------------- helpers ---------------- */

function formatTime(ms) {
  const d = new Date(ms);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

/** Pretty-prints the message when it is JSON, so payloads are readable. */
function prettify(message) {
  const t = message.trim();
  if ((t.startsWith('{') && t.endsWith('}')) || (t.startsWith('[') && t.endsWith(']'))) {
    try {
      return JSON.stringify(JSON.parse(t), null, 2);
    } catch { /* not JSON after all */ }
  }
  return message;
}

function isExpandable(message) {
  return message.length > COLLAPSE_OVER || message.includes('\n') || prettify(message) !== message;
}

function matches(log) {
  if (log.level < state.minLevel) return false;
  if (state.deviceId && String(log.device_id ?? '') !== state.deviceId) return false;
  if (state.search) {
    const hay = `${log.name} ${log.message} ${log.device || ''}`.toLowerCase();
    if (!hay.includes(state.search)) return false;
  }
  return true;
}

function atBottom() {
  return el.logs.scrollHeight - el.logs.scrollTop - el.logs.clientHeight < 40;
}

function scrollToEnd() {
  el.logs.scrollTop = el.logs.scrollHeight;
}

/* ---------------- rendering ---------------- */

function buildRow(log) {
  const row = document.createElement('div');
  row.className = `row lvl-row-${log.level}`;

  const ts = document.createElement('span');
  ts.className = 'ts';
  ts.textContent = formatTime(log.ts);
  ts.title = new Date(log.ts).toISOString();

  const lvl = document.createElement('span');
  lvl.className = `lvl lvl-${log.level}`;
  lvl.textContent = LEVELS[log.level] || log.level;

  const meta = document.createElement('span');
  meta.className = 'meta';
  meta.textContent = (log.name || '').trim();
  if (log.device) {
    const dev = document.createElement('span');
    dev.className = 'dev';
    dev.textContent = ` ${log.device}`;
    meta.appendChild(dev);
  }

  const msg = document.createElement('span');
  msg.className = 'msg';

  if (isExpandable(log.message)) {
    row.classList.add('expandable');
    msg.classList.add('collapsed');
    const caret = document.createElement('span');
    caret.className = 'caret';
    caret.textContent = '▸';
    const body = document.createElement('span');
    body.textContent = log.message;
    msg.append(caret, body);

    row.addEventListener('click', (e) => {
      // Let the user select text without the row collapsing under them.
      if (window.getSelection().toString()) return;
      const open = msg.classList.toggle('collapsed');
      caret.textContent = open ? '▸' : '▾';
      body.textContent = open ? log.message : prettify(log.message);
      e.stopPropagation();
    });
  } else {
    msg.textContent = log.message;
  }

  row.append(ts, lvl, meta, msg);
  return row;
}

/** Full rebuild. Used on filter changes; new lines append incrementally. */
function render() {
  const visible = state.logs.filter(matches);
  el.logs.replaceChildren();

  if (visible.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = state.logs.length
      ? 'No lines match the current filters.'
      : 'Waiting for logs…';
    el.logs.appendChild(empty);
  } else {
    const frag = document.createDocumentFragment();
    for (const log of visible) frag.appendChild(buildRow(log));
    el.logs.appendChild(frag);
  }

  updateStatus(visible.length);
  if (state.follow) scrollToEnd();
}

function updateStatus(visibleCount) {
  el.count.textContent = `${state.logs.length.toLocaleString()} line${state.logs.length === 1 ? '' : 's'}`;
  const filtered = visibleCount !== undefined ? visibleCount : state.logs.filter(matches).length;
  el.shown.textContent =
    filtered === state.logs.length ? '' : `${filtered.toLocaleString()} shown`;
}

function appendLog(log) {
  state.logs.push(log);
  if (log.id > state.lastId) state.lastId = log.id;

  if (state.logs.length > MAX_BUFFER) {
    state.logs.splice(0, state.logs.length - MAX_BUFFER);
    if (!state.paused) { render(); return; }
  }
  if (state.paused) { updateStatus(); return; }

  if (!matches(log)) { updateStatus(); return; }

  const wasAtBottom = atBottom();
  const placeholder = el.logs.querySelector('.empty');
  if (placeholder) placeholder.remove();
  el.logs.appendChild(buildRow(log));

  // Trim rendered nodes in step with the buffer.
  while (el.logs.childElementCount > MAX_BUFFER) el.logs.firstElementChild.remove();

  updateStatus();
  if (state.follow && wasAtBottom) scrollToEnd();
}

/* ---------------- connection ---------------- */

function setConn(cls, text) {
  el.dot.className = `dot ${cls}`;
  el.conn.textContent = text;
}

function connect() {
  if (state.source) state.source.close();
  const source = new EventSource('/logs/stream');
  state.source = source;

  source.onopen = () => setConn('live', 'live');
  source.onmessage = (e) => {
    try { appendLog(JSON.parse(e.data)); } catch { /* ignore malformed frame */ }
  };
  source.onerror = async () => {
    setConn('down', 'reconnecting');
    // EventSource hides the status code, so ask explicitly whether the session
    // expired rather than reconnecting forever against a 401.
    try {
      const res = await fetch('/api/v1/auth/whoami');
      if (res.status === 401) { source.close(); toLogin(); }
    } catch { /* offline; the browser retries on its own */ }
  };
}

function toLogin() {
  location.href = `/login.html?next=${encodeURIComponent(location.pathname)}`;
}

async function loadRecent() {
  const res = await fetch('/api/v1/logs/recent?limit=1000');
  if (res.status === 401) { toLogin(); return; }
  if (!res.ok) throw new Error(`recent failed: ${res.status}`);
  // The API returns newest-first; reverse so the oldest renders at the top.
  const logs = (await res.json()).reverse();
  state.logs = logs;
  for (const l of logs) if (l.id > state.lastId) state.lastId = l.id;
  render();
}

async function loadDevices() {
  try {
    const res = await fetch('/api/v1/devices');
    if (!res.ok) return;
    for (const d of await res.json()) {
      const opt = document.createElement('option');
      opt.value = String(d.id);
      opt.textContent = d.revoked ? `${d.name} (revoked)` : d.name;
      el.device.appendChild(opt);
    }
  } catch { /* filter simply stays on "all devices" */ }
}

/* ---------------- controls ---------------- */

let searchTimer;
el.search.addEventListener('input', () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    state.search = el.search.value.trim().toLowerCase();
    render();
  }, 120);
});

el.level.addEventListener('change', () => {
  state.minLevel = Number(el.level.value);
  render();
});

el.device.addEventListener('change', () => {
  state.deviceId = el.device.value;
  render();
});

function togglePause() {
  state.paused = !state.paused;
  el.pause.textContent = state.paused ? 'Resume' : 'Pause';
  el.pause.classList.toggle('on', state.paused);
  setConn(state.paused ? 'paused' : 'live', state.paused ? 'paused' : 'live');
  // Lines kept arriving into the buffer while paused; show them now.
  if (!state.paused) render();
}
el.pause.addEventListener('click', togglePause);

el.follow.addEventListener('click', () => {
  state.follow = !state.follow;
  el.follow.classList.toggle('on', state.follow);
  if (state.follow) scrollToEnd();
});

el.clear.addEventListener('click', () => {
  state.logs = [];
  render();
});

el.copy.addEventListener('click', async () => {
  const text = JSON.stringify(state.logs.filter(matches), null, 2);
  try {
    await navigator.clipboard.writeText(text);
    const old = el.copy.textContent;
    el.copy.textContent = 'Copied';
    setTimeout(() => { el.copy.textContent = old; }, 1200);
  } catch {
    el.copy.textContent = 'Copy failed';
    setTimeout(() => { el.copy.textContent = 'Copy'; }, 1500);
  }
});

el.logout.addEventListener('click', async (e) => {
  e.preventDefault();
  await fetch('/api/v1/auth/logout', { method: 'POST' });
  toLogin();
});

// Space toggles pause, unless the user is typing in a field.
document.addEventListener('keydown', (e) => {
  if (e.code !== 'Space') return;
  const tag = document.activeElement?.tagName;
  if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return;
  e.preventDefault();
  togglePause();
});

// Scrolling away from the bottom turns following off, the way a terminal does.
el.logs.addEventListener('scroll', () => {
  if (state.follow && !atBottom()) {
    state.follow = false;
    el.follow.classList.remove('on');
  }
});

/* ---------------- boot ---------------- */

(async function main() {
  try {
    const who = await fetch('/api/v1/auth/whoami');
    if (who.status === 401) { toLogin(); return; }
    await Promise.all([loadRecent(), loadDevices()]);
    connect();
  } catch (e) {
    setConn('down', 'offline');
    console.error(e);
  }
})();
