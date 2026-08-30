# Deployment

## Getting it

| Channel | Command |
|---|---|
| Docker | `docker pull sudhis/logger_server:latest` |
| Homebrew (macOS) | `brew install sudhi001/tap/logger-server` |
| crates.io | `cargo install logger_server` |

## Images

| Tag | Base | Image | Idle RSS | Use for |
|---|---|---|---|---|
| `sudhis/logger_server:3.4.0` | `scratch` (static musl) | 5.0 MB | 10.6 MB | Production. The default |
| built from `Dockerfile.glibc` | `distroless/cc` | ~25 MB | higher | When you need glibc, or a shell to debug with |

The default image has no operating system, no shell, and no package manager —
so there is nothing in it to patch, and nothing for an attacker to pivot into.
The trade-off is that you cannot `docker exec` into it.

## It runs as a non-root user

Since 3.4.0 the container runs as uid **65532**, not root. There is no
`/etc/passwd` in a `scratch` image, so the user is purely numeric — the same uid
distroless calls `nonroot`.

Nothing needs root: the server binds port 8080, which is unprivileged, and
writes only to `/data`.

### Upgrading from 3.3.0 or earlier — read this

**A volume created while the image ran as root will not be writable**, and the
server will fail on startup because SQLite cannot create `logs.db-wal` beside
the database. Docker only sets ownership when it *initialises* a volume, so an
existing one keeps whatever it had.

Fix it once, before starting the new version:

```sh
docker run --rm -v logger-data:/data alpine chown -R 65532:65532 /data
```

For a bind mount, chown the host directory instead:

```sh
sudo chown -R 65532:65532 /srv/logger-data
```

A **fresh** volume needs nothing — Docker copies ownership from the image, which
is why `/data` is built with that uid rather than created by `WORKDIR`.

If you would rather not migrate, `--user 0:0` restores the old behaviour, at the
cost of the hardening.

## Docker

```sh
docker run -d --name logger \
  -p 8080:8080 \
  -e LOGGER_ADMIN_TOKEN="$(openssl rand -hex 24)" \
  -v logger-data:/data \
  sudhis/logger_server:3.4.0
```

The volume at `/data` is what makes logs survive a restart.

### Compose

```yaml
services:
  logger:
    image: sudhis/logger_server:3.4.0
    restart: unless-stopped
    ports: ["8080:8080"]
    volumes: ["logger-data:/data"]
    environment:
      LOGGER_ADMIN_TOKEN: ${LOGGER_ADMIN_TOKEN:?set this}
      LOGGER_TRUST_PROXY: "true"
      LOGGER_MAX_ROWS: "500000"
      LOGGER_MAX_AGE_DAYS: "14"

volumes:
  logger-data:
```

## Render

Deploy as a **Web Service** from an existing image.

| Setting | Value |
|---|---|
| Image URL | `docker.io/sudhis/logger_server:3.4.0` |
| Health Check Path | `/healthz` |
| `LOGGER_ADMIN_TOKEN` | a long random string |
| `LOGGER_TRUST_PROXY` | `true` |
| `LOGGER_MAX_ROWS` | `200000` |

Render injects `PORT`, which the server reads automatically. `LOGGER_TRUST_PROXY`
matters because Render fronts the service with a proxy — without it, every
client shares one rate-limit bucket.

> **The free tier has no persistent disk.** Logs are wiped on every restart and
> deploy, and free instances spin down when idle. Fine for debugging sessions;
> export anything you want to keep.

Two Render behaviours worth knowing:

- Changing an environment variable offers **Save only** as well as *Save and
  deploy*. Use it when you do not want an immediate restart.
- Do **not** set the health check path before the running image actually serves
  it, or Render will mark a working service unhealthy.

## Behind nginx

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header Host              $host;
    proxy_set_header X-Real-IP         $remote_addr;
    proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;

    # Required for the live tail: buffering would stall it.
    proxy_buffering off;
    proxy_read_timeout 3600s;
}
```

Then set `LOGGER_TRUST_PROXY=true` so `X-Forwarded-For` is honoured.

## Building it yourself

```sh
docker build --platform linux/amd64 -t logger:local .
docker build --platform linux/amd64 -f Dockerfile.glibc -t logger:glibc .
```

`--platform linux/amd64` matters on Apple Silicon if you are deploying to x86.
The release profile uses fat LTO, so the first emulated build is slow; the
dependency layer is cached afterwards.

## Production checklist

- [ ] `LOGGER_ADMIN_TOKEN` set explicitly to something long and random
- [ ] HTTPS in front, with `LOGGER_COOKIE_SECURE=true`
- [ ] `LOGGER_TRUST_PROXY=true` **only** if a proxy you control sets `X-Forwarded-For`
- [ ] A volume mounted at `/data`, owned by uid 65532, or a deliberate decision that logs are disposable
- [ ] `LOGGER_MAX_ROWS` and `LOGGER_MAX_AGE_DAYS` sized to your disk
- [ ] Health check pointed at `/healthz`
- [ ] `/metrics` scraped, with alerts on `logger_shed_total` and `logger_sse_evicted_total`
- [ ] One device per app build, so you can revoke one without silencing the rest
- [ ] A plan for what happens when a token leaks — revoke, reissue, ship

## Backups

```sh
curl "$URL/api/v1/logs/export?format=ndjson" \
  -H "x-admin-token: $ADMIN" > "logs-$(date +%F).ndjson"
```

Streamed from a cursor, so exporting a very large table does not spike server
memory. If you have filesystem access, copying the SQLite file works too — but
copy the `-wal` and `-shm` files with it, or use `sqlite3 logs.db ".backup"`.

## Upgrading

```sh
docker pull sudhis/logger_server:3.4.0
docker stop logger && docker rm logger
docker run -d --name logger ... sudhis/logger_server:3.4.0
```

Schema changes are applied automatically at startup, and the database is
backward compatible — an older file is migrated in place. Sessions are held in
memory, so everyone is signed out by a restart; devices and their tokens are in
the database and survive.

The server handles `SIGTERM` gracefully: it stops accepting, closes live
streams, and flushes its write queue before exiting. Give it a couple of seconds
to stop rather than `kill -9`, or you lose the last batch.
