'use strict';

const el = {
  form: document.getElementById('form'),
  name: document.getElementById('name'),
  platform: document.getElementById('platform'),
  rows: document.getElementById('rows'),
  empty: document.getElementById('empty'),
  err: document.getElementById('err'),
  newToken: document.getElementById('new-token'),
  tokenValue: document.getElementById('token-value'),
  curlExample: document.getElementById('curl-example'),
  logout: document.getElementById('logout'),
};

function toLogin() {
  location.href = `/login.html?next=${encodeURIComponent(location.pathname)}`;
}

const fmtDate = (ms) => (ms ? new Date(ms).toLocaleString() : '—');

async function load() {
  const res = await fetch('/api/v1/devices');
  if (res.status === 401) { toLogin(); return; }
  if (!res.ok) { el.err.textContent = `Could not load devices (${res.status}).`; return; }

  const devices = await res.json();
  el.rows.replaceChildren();
  el.empty.hidden = devices.length > 0;

  for (const d of devices) {
    const tr = document.createElement('tr');
    if (d.revoked) tr.className = 'revoked';

    const cell = (text, cls) => {
      const td = document.createElement('td');
      td.textContent = text;
      if (cls) td.className = cls;
      return td;
    };

    tr.append(
      cell(d.name),
      cell(d.platform || '—', 'dim'),
      cell(`${d.token_prefix}…`, 'dim'),
      cell(fmtDate(d.created_at), 'dim'),
      cell(d.last_seen ? fmtDate(d.last_seen) : 'never', 'dim'),
    );

    const action = document.createElement('td');
    if (!d.revoked) {
      const btn = document.createElement('button');
      btn.className = 'danger';
      btn.textContent = 'Revoke';
      btn.addEventListener('click', () => revoke(d));
      action.appendChild(btn);
    } else {
      action.textContent = 'revoked';
      action.className = 'dim';
    }
    tr.appendChild(action);
    el.rows.appendChild(tr);
  }
}

async function revoke(device) {
  // Irreversible for that token: the device must be re-registered to log again.
  if (!confirm(`Revoke "${device.name}"? Its token stops working immediately and cannot be restored.`)) {
    return;
  }
  const res = await fetch(`/api/v1/devices/${device.id}`, { method: 'DELETE' });
  if (res.status === 401) { toLogin(); return; }
  if (!res.ok && res.status !== 204) {
    el.err.textContent = `Revoke failed (${res.status}).`;
    return;
  }
  el.err.textContent = '';
  load();
}

el.form.addEventListener('submit', async (e) => {
  e.preventDefault();
  el.err.textContent = '';

  const body = { name: el.name.value.trim() };
  const platform = el.platform.value.trim();
  if (platform) body.platform = platform;
  if (!body.name) { el.err.textContent = 'Device name is required.'; return; }

  const res = await fetch('/api/v1/devices', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (res.status === 401) { toLogin(); return; }
  if (!res.ok) {
    const detail = await res.json().catch(() => ({}));
    el.err.textContent = detail.error || `Could not create device (${res.status}).`;
    return;
  }

  const created = await res.json();
  el.tokenValue.textContent = created.token;
  el.curlExample.textContent =
    `curl -X POST ${location.origin}/api/v1/logs \\\n` +
    `  -H 'Authorization: Bearer ${created.token}' \\\n` +
    `  -H 'content-type: application/json' \\\n` +
    `  -d '{"name":"[app] ","message":"hello","level":2}'`;
  el.newToken.hidden = false;

  el.form.reset();
  load();
});

el.logout.addEventListener('click', async (e) => {
  e.preventDefault();
  await fetch('/api/v1/auth/logout', { method: 'POST' });
  toLogin();
});

load();
