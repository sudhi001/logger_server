# Troubleshooting

## `401 Unauthorized` when sending logs

Since v3 every write needs a device token. Check, in order:

1. **Is there a device?** A fresh server has none, and the log says so at
   startup. Create one on the Devices page or via
   `POST /api/v1/devices`.
2. **Is the header right?** Either
   `Authorization: Bearer lgrd_…` or `X-Device-Token: lgrd_…`.
   Note the space after `Bearer`.
3. **Is it a device token?** Device tokens start `lgrd_`. An admin token
   (`lgra_`) is deliberately not accepted for writing.
4. **Has it been revoked?** Revocation takes effect immediately. Check the
   Devices page; revoked ones are greyed out.
5. **Was it truncated?** Tokens are 37 characters. Shell quoting and
   copy-paste both like to eat them.

The server intentionally gives the same `401` for a missing token and a wrong
one, so a prober cannot use the difference to learn which tokens exist.

## `401` when opening the dashboard

Reads need the admin token or a session. If you never set
`LOGGER_ADMIN_TOKEN`, the server generated one and printed it at startup:

```sh
docker logs logger 2>&1 | grep -i 'generated a temporary'
```

Set the variable explicitly to stop it changing on every restart.

## Login does nothing, or immediately bounces back

Almost always the cookie. If `LOGGER_COOKIE_SECURE` is on but you are on plain
`http://`, the browser accepts the response and silently discards the cookie.

Set `LOGGER_COOKIE_SECURE=false` for local HTTP, and leave it on behind HTTPS.

## Logged out after every deploy

Sessions live in memory, so a restart clears them. That is by design — there is
no key management and nothing to leak — but it means a fresh login per deploy.

If it happens more often than deploys do, your host is restarting the process.
On a free tier that is usually idle spin-down.

## `503` with `Retry-After: 1`

The write queue is full: logs are arriving faster than they can be written.

- Raise `LOGGER_INGEST_QUEUE` to absorb bigger bursts.
- Batch on the client. One request with 500 lines is far cheaper than 500.
- Check `logger_shed_total` in `/metrics` to see how much is actually being lost.

## Batch says `accepted` is less than what I sent

Working as intended. `dropped` tells you how many were refused, and they are the
**last `dropped` entries** of the batch. Resend those. If it happens steadily,
see the `503` advice above.

## Live tail keeps reconnecting

Expected in two cases:

- **You fell behind.** A client that cannot keep up is disconnected rather than
  buffered for. The browser reconnects with `Last-Event-ID` and the gap is
  replayed, so nothing is lost. Watch `logger_sse_evicted_total`.
- **A proxy is buffering.** The server sends `X-Accel-Buffering: no` and a
  keepalive every 15 seconds, which is enough for nginx and Render. Other
  proxies may need response buffering disabled explicitly.

## The container exits immediately after upgrading to 3.4.0

The image now runs as uid 65532 instead of root, and a volume created by an
older version is still owned by root. SQLite cannot create its write-ahead log
next to the database, so the server gives up on startup.

```sh
docker logs logger        # "unable to open database file" or a permission error
docker run --rm -v logger-data:/data alpine chown -R 65532:65532 /data
```

Then start it again. A bind mount needs the same treatment on the host
directory. `--user 0:0` reverts to the old behaviour if you would rather not
migrate.

## Logs vanish after a restart

No volume. The database lives inside the container unless you mount one:

```sh
docker run -v logger-data:/data ... sudhis/logger_server:3.4.0
```

On a host without persistent disks — Render's free tier, for instance — storage
is ephemeral no matter what you mount. Export what you need:

```sh
curl "$URL/api/v1/logs/export?format=ndjson" -H "x-admin-token: $ADMIN" > backup.ndjson
```

## Old logs disappearing on their own

Retention. The defaults keep 1,000,000 rows and 7 days. Raise
`LOGGER_MAX_ROWS` and `LOGGER_MAX_AGE_DAYS`, or set either to `0` to disable —
but then watch your disk.

## `docker login` fails with "no registries found in registries.conf"

Your `docker` is Podman, which will not assume Docker Hub the way the Docker CLI
does. Name the registry:

```sh
docker login docker.io
```

Podman also prefixes locally built images with `localhost/`, so push a
fully-qualified name:

```sh
docker tag localhost/sudhis/logger_server:3.4.0 docker.io/sudhis/logger_server:3.4.0
docker push docker.io/sudhis/logger_server:3.4.0
```

## Memory higher than advertised

`docker stats` reports the cgroup total, which includes page cache for the
SQLite file — real, but reclaimable, and not the process's own memory. For that:

```sh
docker exec logger sh -c 'grep VmRSS /proc/1/status'
```

The scratch image has no shell, so this only works on the glibc variant or by
running the binary in an Alpine container.

Memory does grow with connection count, at roughly 16 KB per open live stream.
That is HTTP per-connection buffers, not per-client log buffering.

## Rate limited when everything comes from one address

Behind a proxy every request appears to come from the proxy, so all your clients
share one bucket. Set `LOGGER_TRUST_PROXY=true` so `X-Forwarded-For` is honoured
— but only if a proxy you trust is overwriting that header.

## Still stuck

Turn up the logs and read them:

```sh
LOGGER_LOG=logger_server=debug,tower_http=debug
```
