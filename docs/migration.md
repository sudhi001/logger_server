# Migrating to v3

v3 is a breaking change. Two things that used to be open now require a
credential.

## What changed

| | Before | v3 |
|---|---|---|
| Sending logs | Anyone with the URL | Needs a device token |
| Reading logs | Anyone with the URL | Needs a login |
| The dashboard | Open | Behind a login |
| `LOGGER_API_KEY` | Optional shared secret | **Removed**, replaced by per-device tokens |
| Log records | `{id, name, message}` | Adds `ts`, `level`, `device_id`, `device` |

Endpoint paths did not change. `POST /logs`, `GET /logs/recent`,
`GET /logs/stream` and the rest all still work — they simply require
authentication now.

## Why it was done this way

The obvious alternative was a grace period: accept unauthenticated writes for a
while, tagged as untrusted, and enforce later. That was considered and rejected,
because a public write endpoint is an open invitation to fill someone's disk,
and a grace period is a deadline nobody ever meets.

Reads were closed at the same time for a reason worth stating plainly: an
unauthenticated log server hands every visitor your stack traces, session
identifiers, and whatever your app happened to print. That is usually a worse
exposure than the writes.

## Upgrading

### 1. Deploy v3 with an admin token

```sh
docker run -d --name logger -p 8080:8080 \
  -e LOGGER_ADMIN_TOKEN="$(openssl rand -hex 24)" \
  -v logger-data:/data \
  sudhis/logger_server:3.2.0
```

Your existing database is migrated in place at startup — a `device_id` column is
added, and old rows keep working with it empty. Nothing is deleted.

Drop `LOGGER_API_KEY`; it is no longer read.

### 2. Create a device per app

One for each build or tester you want to identify and revoke separately. Copy
each token; they are shown once.

### 3. Add the header in your app

```diff
  POST /api/v1/logs
+ Authorization: Bearer lgrd_your_device_token
  Content-Type: application/json
```

That is the whole client change. Everything else about the request is the same.

Now is a good moment to switch to batching — see [clients.md](clients.md) for
loggers that already do it.

### 4. Ship, then watch

Old builds will start returning `401`. **Their logs are lost, not queued**, so
until users update you are blind to them. Two ways to soften that:

- Roll the new build out before you deploy v3, if your client can carry a token
  the old server ignores.
- Keep the old server running on another port until the old builds age out.

The Devices page shows **Last seen** per device, which is the quickest way to
confirm a build has actually migrated.

## Existing logs

Preserved. They keep their ids and messages, and show with no device attribution
since there was none to record. `ts` is backfilled as empty for rows written
before the column existed; the dashboard falls back to ordering by id.

## Rolling back

v3 only adds a column, so an older binary will still open the database and
ignore it. You would lose device authentication entirely, which is the thing you
upgraded for.

## v1 (Kotlin) to v3

If you are coming from the original Spring Boot server, the database is H2 and
cannot be read by v3, which uses SQLite. Options:

- **Start fresh.** Usually right — old debug logs are rarely worth migrating.
- **Export and replay.** Dump the H2 table with `SCRIPT TO`, transform the rows
  to JSON, and `POST` them to `/api/v1/logs/batch` with a device token and an
  explicit `ts` on each record.

The API shape is otherwise unchanged, so clients only need the new header.
