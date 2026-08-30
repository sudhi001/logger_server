<h1 align="center">Remote Logger</h1>

<p align="center">
  See what your mobile app is doing on someone else's phone — live, in your browser.
</p>

<p align="center">
  <a href="https://logger-server-z5w8.onrender.com"><b>Live demo</b></a> ·
  <a href="docs/quickstart.md">Quick start</a> ·
  <a href="docs/clients.md">Connect your app</a> ·
  <a href="docs/agents.md">Connect an AI agent</a> ·
  <a href="docs/alerting.md">Alerting</a> ·
  <a href="docs/api.md">API</a>
</p>

![The dashboard: three devices logging at once, colour-coded by level](docs/images/dashboard.jpg)

---

## The problem

Your app works on your machine. Then a tester on the other side of the country
opens it, taps three things, and it crashes.

You ask what happened. They say *"it just closed"*. The log line that would have
told you exactly what went wrong was printed — for a fraction of a second, on a
phone you cannot reach — and then it was gone. `adb logcat` needs a cable. Xcode
needs the device on your desk. Neither is any use here.

![Without a remote logger the log line dies on the device; with one it reaches your browser](docs/images/why.svg)

So you add print statements, ship another build, and wait a day to find out you
guessed wrong.

## What this is

A small server **you** run. Your app sends its log lines to it over the
internet, and you watch them arrive live in a browser tab — the same way you'd
watch `logcat`, except the phone can be anywhere in the world.

That's the whole idea. Everything else is detail.

**In one sentence for each audience:**

- *If you build mobile apps* — it's remote `logcat` for builds you can't plug in.
- *If you run servers* — it's a single 4.9 MB static binary with a SQLite file,
  an SSE live tail, and per-device bearer tokens.
- *If you're neither* — it's a notebook your app writes to, that you can read
  from anywhere, that nobody else can open.

## How it works

![Three steps: your app posts a log with its token, the server stores and fans it out, your browser shows it live](docs/images/how-it-works.svg)

Three moving parts, and that's all:

1. **Your app sends.** A few lines of code — [copy them from here](docs/clients.md)
   — collect log lines and post them to your server. Each app build carries its
   own token, so you always know which phone a line came from.
2. **The server stores and fans out.** It writes each line to a database file
   and, at the same moment, pushes it to anyone watching. Those two happen
   independently, so a slow disk never delays your live view.
3. **You watch.** Open the page. Lines appear as they happen — no refresh, no
   polling.

## What you can actually do with it

**Read a crash with its full stack trace**, and pretty-print a JSON payload,
without leaving the page. Click any line to expand it:

![A crash expanded to show its full stack trace, and a JSON payload pretty-printed](docs/images/dashboard-expanded.jpg)

**Cut the noise down to what matters.** Filter by level, by device, or by any
text in the message. Here, three chatty devices reduced to only the warnings and
errors:

![The same stream filtered to warnings and errors only: 29 lines, 10 shown](docs/images/dashboard-filtered.jpg)

**Search everything you have ever logged.** Full-text search across all history
— not just what is on screen — filtered by level, device, tag or time range.

**Get told when something breaks.** Alert rules watch the stream and post to
Slack, Discord, PagerDuty or any webhook — with a threshold and cooldown, so a
crash loop sends one message rather than a thousand:

![The alerts page, with one rule showing a failing webhook](docs/images/alerts.jpg)

**Give every tester their own credential.** One per build, per phone, per person
— so you can see whose logs you're reading, and cut one off without disturbing
anyone else:

![The devices page, showing a newly created token and a list of registered devices](docs/images/devices.jpg)

Plus the things you'd expect from a log tail: pause to read something (or hit
space), follow-the-tail that switches off when you scroll up, clear the view,
and copy what's on screen as JSON.

## Let an AI agent read your logs

The server speaks the **Model Context Protocol**, so an assistant can
investigate for you instead of you scrolling:

> *"Why did checkout fail on Priya's phone this afternoon?"*
> *"Is this crash hitting everyone, or one device?"*
> *"What happened in the thirty seconds before that NullPointerException?"*

Point Claude Code at it:

```sh
claude mcp add --transport http logger https://your-logger.example.com/mcp \
  --header "Authorization: Bearer $LOGGER_ADMIN_TOKEN"
```

Or add it to any MCP client that speaks HTTP:

```json
{
  "mcpServers": {
    "logger": {
      "type": "http",
      "url": "https://your-logger.example.com/mcp",
      "headers": { "Authorization": "Bearer lgra_your_admin_token" }
    }
  }
}
```

The agent gets tools to search all history, pull the lines around any log,
aggregate by level and device, and list devices — so it answers by looking
things up rather than guessing from the last thousand lines.

> **Agents read text your app was given, which means text an attacker may
> influence.** By default the agent can also create and revoke device tokens.
> Set `LOGGER_MCP_MODE=read` to make it query-only, which is the right choice
> unless you specifically want an agent provisioning devices.
> [Full guide, including the prompt-injection trade-off →](docs/agents.md)

## Quick start

One command:

```sh
docker run -d --name logger -p 8080:8080 \
  -e LOGGER_ADMIN_TOKEN=pick-a-long-random-string \
  -v logger-data:/data \
  sudhis/logger_server:3.2.0
```

Open <http://localhost:8080>, sign in with that token, go to **Devices**, and
create one. Copy the token it gives you — it's shown once — then prove it works:

```sh
curl -X POST http://localhost:8080/api/v1/logs \
  -H "Authorization: Bearer <your-device-token>" \
  -H 'content-type: application/json' \
  -d '{"name":"[MyApp] ","message":"hello","level":2}'
```

That line appears in your browser immediately. Now wire it into your app:

| | | |
|---|---|---|
| [Android / Kotlin](docs/clients.md#android--kotlin) | [iOS / Swift](docs/clients.md#ios--swift) | [Flutter / Dart](docs/clients.md#flutter--dart) |
| [React Native](docs/clients.md#react-native--javascript) | [Node.js](docs/clients.md#nodejs) | [Python](docs/clients.md#python) |
| [Go](docs/clients.md#go) | [PHP](docs/clients.md#php) | [C](docs/clients.md#c) / [C++](docs/clients.md#c-1) |

Every one of those buffers and batches, so you aren't making an HTTP request per
log line. Eight of the ten were compiled and run against a live server before
being written down.

Longer walkthrough: **[docs/quickstart.md](docs/quickstart.md)**.

## Who can see what

Nothing is open to the public. There are two kinds of credential, and they do
very different things:

![Device tokens can only write logs; the admin token or a dashboard session can read and manage](docs/images/auth.svg)

The short version: **the token inside your app can only write.** It cannot read
your logs, cannot see other devices, and cannot manage anything. Reading needs a
separate credential that never leaves your hands.

> **Before you ship a token in a public app store release**, read
> [the note on that](docs/clients.md#a-word-on-putting-tokens-in-your-app). It is
> fine for internal, QA and beta builds — which is what this tool is for — but a
> token in a downloadable binary can be extracted, and that deserves a
> deliberate decision rather than a surprise.

## Why it's this small

The server was originally Spring Boot on the JVM and used 250–400 MB of RAM
sitting completely idle. It was rewritten in Rust, and now the whole thing —
web server, database, live streaming, dashboard — fits in about 11 MB.

That matters for one practical reason: **it runs on the free tier of anything.**

| | Before (JVM) | Now (Rust) |
|---|---|---|
| Memory, idle | ~250–400 MB | **15.1 MB** |
| Memory, 400 people watching | — | **~21 MB** |
| Startup | 2–5 seconds | **~25 ms** |
| Download size | 271 MB | **~5 MB** compressed |
| Logs accepted per second | ~2–5 k | **~38 k** (with search indexing) |

Measured, not estimated — `VmRSS` read from `/proc` on the real image. If you
want to know *how*, [docs/architecture.md](docs/architecture.md) explains the
three ideas that do most of the work.

Those numbers went up in 3.2.0: adding webhook alerting means shipping a TLS
stack, which cost about 5 MB of image and 4 MB of resident memory. Worth it to
be told when something breaks, but it is a real cost and not worth hiding.

## Documentation

| Guide | What's in it |
|---|---|
| [Quick start](docs/quickstart.md) | Running, signed in, and logging in a couple of minutes |
| [Connect your app](docs/clients.md) | Ready-to-paste loggers for ten languages |
| [API reference](docs/api.md) | Every endpoint, parameter and status code |
| [Configuration](docs/configuration.md) | Every setting, and when you'd change it |
| [Connect an AI agent](docs/agents.md) | MCP setup, the tools, and the access model |
| [Alerting](docs/alerting.md) | Webhook rules, thresholds, and outbound safety |
| [Deployment](docs/deployment.md) | Docker, Compose, Render, nginx, production checklist |
| [Architecture](docs/architecture.md) | How it works inside, and why it's built this way |
| [Migrating to v3](docs/migration.md) | The breaking changes, and how to move |
| [Troubleshooting](docs/troubleshooting.md) | The errors you're most likely to hit |

## Running it yourself

**macOS, without Docker:**

```sh
brew install sudhi001/tap/logger-server
logger-server
```

**From source** — Rust 1.80 or newer. No JVM, no Gradle:

```sh
cargo run --release      # starts on :8080
cargo test               # 58 tests
```

Or take the image from [Docker Hub](https://hub.docker.com/r/sudhis/logger_server).

## Licence

MIT. Run it, change it, ship it.
