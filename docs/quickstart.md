# Quick start

Getting from nothing to a live log tail. Should take about two minutes.

## 1. Run the server

```sh
docker run -d --name logger -p 8080:8080 \
  -e LOGGER_ADMIN_TOKEN=pick-a-long-random-string \
  -v logger-data:/data \
  sudhis/logger_server:3.0.0
```

Two flags worth understanding:

- **`LOGGER_ADMIN_TOKEN`** is your dashboard password. Pick something long and
  random. If you leave it out the server generates one and prints it to its log
  — usable, but it changes on every restart, which signs you out each time.
- **`-v logger-data:/data`** is what makes logs survive a restart. Without it
  the database lives inside the container and disappears with it.

Prefer to run it from source? `cargo run --release` does the same thing.

Check it came up:

```sh
curl localhost:8080/healthz     # -> ok
```

## 2. Sign in

Open <http://localhost:8080>. You will be sent to a login page — nothing is
readable without signing in. Enter the admin token.

## 3. Register a device

"Device" means one app build, one phone, or one tester — whatever you want to be
able to identify and revoke separately.

Go to **Devices** → enter a name like `Pixel 8 — QA` → **Create device**.

You will get a token like `lgrd_nZQa6MFVrSzkWrAGQRmSqWnScurnWvgi`. **Copy it
now.** Only its hash is stored, so it cannot be shown to you again. If you lose
it, revoke the device and make another.

Prefer the API?

```sh
curl -X POST http://localhost:8080/api/v1/devices \
  -H "x-admin-token: $LOGGER_ADMIN_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"Pixel 8 — QA","platform":"Android 15"}'
```

## 4. Send a log

```sh
curl -X POST http://localhost:8080/api/v1/logs \
  -H "Authorization: Bearer lgrd_your_device_token" \
  -H 'content-type: application/json' \
  -d '{"name":"[MyApp] ","message":"hello","level":2}'
```

It appears in the browser immediately, tagged with the device that sent it.

Only `message` is really required. `name` is a free-text tag — most people use
it for a subsystem, like `[Network]` — and `level` runs 0–4:

| Level | 0 | 1 | 2 | 3 | 4 |
|---|---|---|---|---|---|
| | trace | debug | **info** (default) | warn | error |

## 5. Wire it into your app

Copy a logger from **[clients.md](clients.md)** — Kotlin, Swift, Dart,
JavaScript, Node, or Python. They buffer and batch rather than making one
request per line, which matters on mobile.

## Using the dashboard

| | |
|---|---|
| **Search box** | Filters message, tag, and device as you type |
| **Level / device dropdowns** | Narrow to what you care about |
| **Pause** (or `space`) | Freeze the view to read something; lines keep arriving behind it |
| **Follow** | Auto-scroll to newest. Scrolling up turns it off, like a terminal |
| **Clear** | Empties the view without deleting anything on the server |
| **Copy** | Puts the currently visible lines on your clipboard as JSON |
| **Click a line** | Expands it — pretty-prints JSON, and preserves stack-trace newlines |

The view holds the most recent 5,000 lines. Older history is still on the server
and reachable through [the API](api.md).

## Next

- [Connect your app](clients.md)
- [Deploy it somewhere](deployment.md)
- [Tune it](configuration.md)
