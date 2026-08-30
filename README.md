# Remote Logger for Mobile App Developers

Ship logs from your mobile app to a server you control, and watch them stream
live in your browser. Self-hosted, one static binary, ~11 MB of RAM.

**[Live demo](https://logger-server-z5w8.onrender.com)** ·
[Quick start](docs/quickstart.md) ·
[Connect your app](docs/clients.md) ·
[API reference](docs/api.md)

---

## Why this exists

Debugging a mobile app on a real device is miserable. The device is not attached
to your machine, `adb logcat` is not available over the tester's WiFi, and the
bug happened twenty minutes ago on someone else's phone.

This gives every build a place to send its logs, and gives you a live tail of
them in a browser tab.

- **Per-device tokens** — each build or tester gets its own credential, so you
  can tell whose logs you are reading and cut one off without touching the rest.
- **Live tail with real filters** — search, level and device filters, pause,
  follow, expandable JSON and stack traces.
- **Small enough to run anywhere** — a 4.9 MB container that idles at 11 MB of
  RAM, so a free-tier box is plenty.
- **Your data stays yours** — one binary, one SQLite file, no third party.

## Quick start

```sh
docker run -d --name logger -p 8080:8080 \
  -e LOGGER_ADMIN_TOKEN=pick-a-long-random-string \
  -v logger-data:/data \
  sudhis/logger_server:3.0.0
```

Open <http://localhost:8080>, sign in with that token, go to **Devices**, and
create one. Copy the token it shows you — it is displayed once — then:

```sh
curl -X POST http://localhost:8080/api/v1/logs \
  -H "Authorization: Bearer <device-token>" \
  -H 'content-type: application/json' \
  -d '{"name":"[MyApp] ","message":"hello from curl","level":2}'
```

The line appears in your browser immediately. Full walkthrough:
**[docs/quickstart.md](docs/quickstart.md)**.

## Connect your app

Ready-to-paste loggers — buffered and batched, so you are not making one HTTP
request per log line:

| | |
|---|---|
| [Android / Kotlin](docs/clients.md#android--kotlin) | [iOS / Swift](docs/clients.md#ios--swift) |
| [Flutter / Dart](docs/clients.md#flutter--dart) | [React Native / JS](docs/clients.md#react-native--javascript) |
| [Node.js](docs/clients.md#nodejs) | [Python](docs/clients.md#python) |

> **Read [the note on shipping tokens in app binaries](docs/clients.md#a-word-on-putting-tokens-in-your-app)
> before you put a token in a public release.** It is fine for internal and QA
> builds; a public store build is a different question.

## Documentation

| Guide | What is in it |
|---|---|
| [Quick start](docs/quickstart.md) | Running, signed in, and logging in a couple of minutes |
| [Connect your app](docs/clients.md) | Client code for six languages, plus batching advice |
| [API reference](docs/api.md) | Every endpoint, parameter, and status code |
| [Configuration](docs/configuration.md) | Every environment variable and what it changes |
| [Deployment](docs/deployment.md) | Docker, Render, and a production checklist |
| [Architecture](docs/architecture.md) | How it works and why it is built this way |
| [Migrating to v3](docs/migration.md) | The breaking changes, and how to move |
| [Troubleshooting](docs/troubleshooting.md) | The errors you are most likely to hit |

## Performance

The server was rewritten from Spring Boot / Kotlin to Rust. Measured on the
`linux/amd64` image, `VmRSS` read from `/proc`:

| | Kotlin / Spring Boot | Rust |
|---|---|---|
| Idle RSS | ~250–400 MB | **11.1 MB** |
| 400 live tails | — | **17.2 MB** (~16 KB/connection) |
| Cold start | 2–5 s | **~25 ms** |
| Image size | 271 MB | **2.5 MB** compressed, 4.9 MB on disk |
| Batched ingest | ~2–5 k/s | **~50 k rows/s** |
| `GET /logs` on a huge table | loads it all into heap | streams, flat memory |

The reasoning behind those numbers is in
[docs/architecture.md](docs/architecture.md).

## Building from source

Rust 1.80 or newer. No JVM, no Gradle.

```sh
cargo run --release      # starts on :8080
cargo test               # 30 tests
```

## Licence

MIT.
