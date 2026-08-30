const form = document.getElementById('form');
const tokenInput = document.getElementById('token');
const err = document.getElementById('err');

form.addEventListener('submit', async (e) => {
  e.preventDefault();
  err.textContent = '';
  const token = tokenInput.value.trim();
  if (!token) { err.textContent = 'Token required.'; return; }

  try {
    const res = await fetch('/api/v1/auth/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ token }),
    });
    if (res.ok) {
      // Land wherever the user was originally headed, if anywhere.
      const next = new URLSearchParams(location.search).get('next');
      location.href = next && next.startsWith('/') ? next : '/';
    } else if (res.status === 401) {
      err.textContent = 'That token was not accepted.';
      tokenInput.select();
    } else {
      err.textContent = `Unexpected response (${res.status}).`;
    }
  } catch (e) {
    err.textContent = 'Could not reach the server.';
  }
});
