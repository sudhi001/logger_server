'use strict';

const LEVELS = ['trace', 'debug', 'info', 'warn', 'error'];

const el = {
  form: document.getElementById('form'),
  rows: document.getElementById('rows'),
  empty: document.getElementById('empty'),
  err: document.getElementById('err'),
  logout: document.getElementById('logout'),
};

const field = (id) => document.getElementById(id).value.trim();
const toLogin = () => {
  location.href = `/login.html?next=${encodeURIComponent(location.pathname)}`;
};
const fmtDate = (ms) => (ms ? new Date(ms).toLocaleString() : 'never');

/** "3 errors in 60s, then quiet for 15m" — the rule in one readable line. */
function describeTrigger(r) {
  const secs = (s) => (s >= 3600 ? `${Math.round(s / 3600)}h` : s >= 60 ? `${Math.round(s / 60)}m` : `${s}s`);
  const n = r.threshold === 1 ? 'first match' : `${r.threshold} in ${secs(r.window_secs)}`;
  return r.cooldown_secs > 0 ? `${n}, then quiet ${secs(r.cooldown_secs)}` : n;
}

function describeMatch(r) {
  const bits = [`${LEVELS[r.min_level] || r.min_level}+`];
  if (r.name_filter) bits.push(`tag ${r.name_filter.trim()}`);
  if (r.contains) bits.push(`"${r.contains}"`);
  if (r.device_id) bits.push(`device ${r.device_id}`);
  return bits.join(' · ');
}

async function load() {
  const res = await fetch('/api/v1/alerts');
  if (res.status === 401) { toLogin(); return; }
  if (!res.ok) { el.err.textContent = `Could not load alerts (${res.status}).`; return; }

  const rules = await res.json();
  el.rows.replaceChildren();
  el.empty.hidden = rules.length > 0;

  for (const r of rules) {
    const tr = document.createElement('tr');
    if (!r.enabled) tr.className = 'revoked';

    const cell = (text, cls) => {
      const td = document.createElement('td');
      td.textContent = text;
      if (cls) td.className = cls;
      return td;
    };

    tr.append(
      cell(r.name),
      cell(describeMatch(r), 'dim'),
      cell(describeTrigger(r), 'dim'),
      cell(`${r.format}${r.signed ? ' · signed' : ''}`, 'dim'),
      cell(`${fmtDate(r.last_fired_at)}${r.fire_count ? ` (${r.fire_count}×)` : ''}`, 'dim'),
    );

    // Status is the useful column: a webhook that has been failing silently
    // is worse than no webhook at all.
    const status = document.createElement('td');
    if (r.last_error) {
      status.textContent = `failing: ${r.last_error}`;
      status.style.color = 'var(--error)';
      status.title = r.last_error;
    } else if (!r.enabled) {
      status.textContent = 'disabled';
      status.className = 'dim';
    } else {
      status.textContent = r.fire_count ? 'delivering' : 'armed';
      status.style.color = 'var(--ok)';
    }
    tr.appendChild(status);

    const actions = document.createElement('td');
    actions.style.whiteSpace = 'nowrap';

    const test = document.createElement('button');
    test.textContent = 'Test';
    test.title = 'Send a sample now, ignoring threshold and cooldown';
    test.addEventListener('click', () => sendTest(r, test));

    const toggle = document.createElement('button');
    toggle.textContent = r.enabled ? 'Disable' : 'Enable';
    toggle.addEventListener('click', () => setEnabled(r, !r.enabled));

    const del = document.createElement('button');
    del.className = 'danger';
    del.textContent = 'Delete';
    del.addEventListener('click', () => remove(r));

    actions.append(test, ' ', toggle, ' ', del);
    tr.appendChild(actions);
    el.rows.appendChild(tr);
  }
}

async function sendTest(rule, btn) {
  const original = btn.textContent;
  btn.textContent = 'Sending…';
  const res = await fetch(`/api/v1/alerts/${rule.id}/test`, { method: 'POST' });
  if (res.status === 401) { toLogin(); return; }
  btn.textContent = res.ok ? 'Sent' : 'Failed';
  // Delivery is asynchronous, so reload to surface whatever came back.
  setTimeout(() => { btn.textContent = original; load(); }, 2000);
}

async function setEnabled(rule, enabled) {
  const res = await fetch(`/api/v1/alerts/${rule.id}`, {
    method: 'PATCH',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ enabled }),
  });
  if (res.status === 401) { toLogin(); return; }
  load();
}

async function remove(rule) {
  if (!confirm(`Delete "${rule.name}"? Nothing will notify you when it would have fired.`)) return;
  const res = await fetch(`/api/v1/alerts/${rule.id}`, { method: 'DELETE' });
  if (res.status === 401) { toLogin(); return; }
  load();
}

el.form.addEventListener('submit', async (e) => {
  e.preventDefault();
  el.err.textContent = '';

  const body = {
    name: field('name'),
    url: field('url'),
    format: field('format'),
    min_level: Number(field('min_level')),
    threshold: Number(field('threshold')) || 1,
    window_secs: Number(field('window_secs')) || 300,
    cooldown_secs: Number(field('cooldown_secs')) || 0,
  };
  for (const k of ['contains', 'name_filter', 'secret']) {
    if (field(k)) body[k] = field(k);
  }

  const res = await fetch('/api/v1/alerts', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (res.status === 401) { toLogin(); return; }
  if (!res.ok) {
    const detail = await res.json().catch(() => ({}));
    el.err.textContent = detail.error || `Could not create the alert (${res.status}).`;
    return;
  }
  el.form.reset();
  document.getElementById('threshold').value = '1';
  document.getElementById('window_secs').value = '300';
  document.getElementById('cooldown_secs').value = '900';
  load();
});

el.logout.addEventListener('click', async (e) => {
  e.preventDefault();
  await fetch('/api/v1/auth/logout', { method: 'POST' });
  toLogin();
});

load();
