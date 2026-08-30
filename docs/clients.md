# Connect your app

Copy-paste loggers for six platforms. Every one of them **buffers and batches**
rather than firing an HTTP request per log line — on a phone, one request per
line will flatten the battery and fall over the moment the network hiccups.

All of them follow the same three rules:

1. **Batch.** Collect lines in memory, flush every couple of seconds or once
   enough have piled up, and send them in one request to `/api/v1/logs/batch`.
2. **Never throw.** A logger that crashes the app it is instrumenting is worse
   than no logger. Every one of these swallows its errors.
3. **Bound the buffer.** If the network is down, drop the oldest lines rather
   than growing until the app is killed.

**Mobile:** [Kotlin](#android--kotlin) · [Swift](#ios--swift) · [Dart](#flutter--dart) · [React Native](#react-native--javascript)

**Backend & systems:** [Go](#go) · [Node](#nodejs) · [Python](#python) · [PHP](#php) · [C](#c) · [C++](#c-1)

**Shell:** [curl](#curl)

> Every sample below except Kotlin and PHP was compiled and run against a real
> server while writing this page, and each one's output was checked on the other
> end — including that the hand-written JSON escaping in the C and C++ clients
> survives quotes and newlines. Kotlin and PHP are written to the same shape but
> were not executed, since neither toolchain was available; treat those two as
> reviewed rather than tested.

---

## Where the token comes from

Create one per app build on the **Devices** page. It is shown once, with a
ready-to-paste example:

![The devices page with a freshly created token](images/devices.jpg)

---

## A word on putting tokens in your app

A device token embedded in a shipped app binary **can be extracted**. Anyone
willing to unzip your APK or IPA and run `strings` will find it, and can then
write logs as that device.

For the job this tool is built for — debugging your own internal, QA, and beta
builds — that is fine. The token writes logs and nothing else: it cannot read
them, cannot list other devices, and you can revoke it in one click the moment
it is abused.

For a **public store release**, think it through:

- Ship the logger in debug/internal builds only, so no token reaches production.
- Or have your own backend hand a token to the app at runtime after the user
  authenticates, instead of baking one in.
- Give each build its own device, so revoking one does not silence the rest.
- Watch **Last seen** on the Devices page. Traffic from a device you retired is
  your signal that its token leaked.

Reads are a different matter and are already protected: viewing logs needs the
admin token or a dashboard session, neither of which belongs in an app.

---

## Android / Kotlin

Uses OkHttp and coroutines. Add to `build.gradle.kts`:

```kotlin
implementation("com.squareup.okhttp3:okhttp:4.12.0")
implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
```

And in `AndroidManifest.xml`:

```xml
<uses-permission android:name="android.permission.INTERNET" />
```

```kotlin
import kotlinx.coroutines.*
import okhttp3.*
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicInteger

object RemoteLogger {
    private const val FLUSH_EVERY_MS = 2_000L
    private const val FLUSH_AT_SIZE = 50
    private const val MAX_BUFFERED = 1_000

    private lateinit var endpoint: String
    private lateinit var token: String

    private val queue = ConcurrentLinkedQueue<JSONObject>()
    private val queued = AtomicInteger(0)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val http = OkHttpClient()
    private val json = "application/json".toMediaType()

    fun start(baseUrl: String, deviceToken: String) {
        endpoint = "${baseUrl.trimEnd('/')}/api/v1/logs/batch"
        token = deviceToken
        scope.launch {
            while (isActive) {
                delay(FLUSH_EVERY_MS)
                flush()
            }
        }
    }

    fun trace(tag: String, message: String) = log(0, tag, message)
    fun debug(tag: String, message: String) = log(1, tag, message)
    fun info(tag: String, message: String) = log(2, tag, message)
    fun warn(tag: String, message: String) = log(3, tag, message)
    fun error(tag: String, message: String, t: Throwable? = null) =
        log(4, tag, if (t == null) message else "$message\n${t.stackTraceToString()}")

    fun log(level: Int, tag: String, message: String) {
        // Drop the oldest rather than grow without bound while offline.
        while (queued.get() >= MAX_BUFFERED) {
            queue.poll()?.let { queued.decrementAndGet() }
        }
        queue.add(
            JSONObject()
                .put("name", tag)
                .put("message", message)
                .put("level", level)
                .put("ts", System.currentTimeMillis())
        )
        if (queued.incrementAndGet() >= FLUSH_AT_SIZE) {
            scope.launch { flush() }
        }
    }

    /** Call from Activity.onPause / onStop so a backgrounded app is not lost. */
    fun flushNow() {
        scope.launch { flush() }
    }

    private fun flush() {
        if (queue.isEmpty()) return
        val batch = JSONArray()
        while (batch.length() < 500) {
            val next = queue.poll() ?: break
            queued.decrementAndGet()
            batch.put(next)
        }
        if (batch.length() == 0) return

        val request = Request.Builder()
            .url(endpoint)
            .addHeader("Authorization", "Bearer $token")
            .post(batch.toString().toRequestBody(json))
            .build()

        try {
            // A logger must never take down the app it is instrumenting.
            http.newCall(request).execute().use { /* response ignored */ }
        } catch (_: Exception) {
        }
    }
}
```

```kotlin
// Application.onCreate
RemoteLogger.start("https://logs.example.com", BuildConfig.LOGGER_TOKEN)

RemoteLogger.info("[Auth] ", "Signed in as $userId")
RemoteLogger.error("[Net] ", "Upload failed", exception)
```

Keep the token out of source control — put it in `local.properties` and surface
it through `BuildConfig`.

---

## iOS / Swift

No dependencies; `URLSession` is enough.

```swift
import Foundation

final class RemoteLogger {
    static let shared = RemoteLogger()

    private var endpoint: URL!
    private var token: String = ""

    private var buffer: [[String: Any]] = []
    private let lock = DispatchQueue(label: "remote-logger")
    private var timer: DispatchSourceTimer?

    private let flushEvery: TimeInterval = 2
    private let flushAtSize = 50
    private let maxBuffered = 1_000

    func start(baseUrl: String, deviceToken: String) {
        endpoint = URL(string: baseUrl.hasSuffix("/")
            ? baseUrl + "api/v1/logs/batch"
            : baseUrl + "/api/v1/logs/batch")!
        token = deviceToken

        let t = DispatchSource.makeTimerSource(queue: lock)
        t.schedule(deadline: .now() + flushEvery, repeating: flushEvery)
        t.setEventHandler { [weak self] in self?.flush() }
        t.resume()
        timer = t
    }

    func trace(_ tag: String, _ message: String) { log(0, tag, message) }
    func debug(_ tag: String, _ message: String) { log(1, tag, message) }
    func info(_ tag: String, _ message: String)  { log(2, tag, message) }
    func warn(_ tag: String, _ message: String)  { log(3, tag, message) }
    func error(_ tag: String, _ message: String) { log(4, tag, message) }

    func log(_ level: Int, _ tag: String, _ message: String) {
        let entry: [String: Any] = [
            "name": tag,
            "message": message,
            "level": level,
            "ts": Int(Date().timeIntervalSince1970 * 1000),
        ]
        lock.async {
            self.buffer.append(entry)
            // Drop the oldest rather than grow without bound while offline.
            if self.buffer.count > self.maxBuffered {
                self.buffer.removeFirst(self.buffer.count - self.maxBuffered)
            }
            if self.buffer.count >= self.flushAtSize { self.flush() }
        }
    }

    /// Call from applicationDidEnterBackground.
    func flushNow() { lock.async { self.flush() } }

    // Must be called on `lock`.
    private func flush() {
        guard !buffer.isEmpty else { return }
        let batch = Array(buffer.prefix(500))
        buffer.removeFirst(batch.count)

        guard let body = try? JSONSerialization.data(withJSONObject: batch) else { return }
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = body

        // Errors are deliberately ignored: logging must not disrupt the app.
        URLSession.shared.dataTask(with: request).resume()
    }
}
```

```swift
RemoteLogger.shared.start(baseUrl: "https://logs.example.com",
                          deviceToken: Secrets.loggerToken)

RemoteLogger.shared.info("[Auth] ", "Signed in as \(userId)")
RemoteLogger.shared.error("[Net] ", "Upload failed: \(error)")
```

---

## Flutter / Dart

```yaml
dependencies:
  http: ^1.2.0
```

```dart
import 'dart:async';
import 'dart:convert';
import 'package:http/http.dart' as http;

class RemoteLogger {
  RemoteLogger._();
  static final RemoteLogger instance = RemoteLogger._();

  static const _flushEvery = Duration(seconds: 2);
  static const _flushAtSize = 50;
  static const _maxBuffered = 1000;

  late Uri _endpoint;
  late String _token;
  final List<Map<String, dynamic>> _buffer = [];
  Timer? _timer;

  void start({required String baseUrl, required String deviceToken}) {
    final base = baseUrl.endsWith('/')
        ? baseUrl.substring(0, baseUrl.length - 1)
        : baseUrl;
    _endpoint = Uri.parse('$base/api/v1/logs/batch');
    _token = deviceToken;
    _timer?.cancel();
    _timer = Timer.periodic(_flushEvery, (_) => flush());
  }

  void trace(String tag, String message) => log(0, tag, message);
  void debug(String tag, String message) => log(1, tag, message);
  void info(String tag, String message) => log(2, tag, message);
  void warn(String tag, String message) => log(3, tag, message);
  void error(String tag, String message, [Object? e, StackTrace? st]) =>
      log(4, tag, [message, if (e != null) '$e', if (st != null) '$st'].join('\n'));

  void log(int level, String tag, String message) {
    _buffer.add({
      'name': tag,
      'message': message,
      'level': level,
      'ts': DateTime.now().millisecondsSinceEpoch,
    });
    // Drop the oldest rather than grow without bound while offline.
    if (_buffer.length > _maxBuffered) {
      _buffer.removeRange(0, _buffer.length - _maxBuffered);
    }
    if (_buffer.length >= _flushAtSize) flush();
  }

  Future<void> flush() async {
    if (_buffer.isEmpty) return;
    final batch = _buffer.take(500).toList();
    _buffer.removeRange(0, batch.length);
    try {
      await http.post(
        _endpoint,
        headers: {
          'Authorization': 'Bearer $_token',
          'content-type': 'application/json',
        },
        body: jsonEncode(batch),
      );
    } catch (_) {
      // Logging must never surface an error into the app.
    }
  }

  void dispose() => _timer?.cancel();
}
```

```dart
RemoteLogger.instance.start(
  baseUrl: 'https://logs.example.com',
  deviceToken: const String.fromEnvironment('LOGGER_TOKEN'),
);

RemoteLogger.instance.info('[Auth] ', 'Signed in as $userId');

// Route Flutter's own errors there too.
FlutterError.onError = (details) {
  RemoteLogger.instance.error('[Flutter] ', details.exceptionAsString(),
      details.exception, details.stack);
};
```

Pass the token at build time: `flutter build apk --dart-define=LOGGER_TOKEN=lgrd_…`

---

## React Native / JavaScript

Works unchanged in React Native, a browser, or any modern JS runtime.

```js
export class RemoteLogger {
  constructor(baseUrl, deviceToken, {
    flushEveryMs = 2000, flushAtSize = 50, maxBuffered = 1000,
  } = {}) {
    this.endpoint = `${baseUrl.replace(/\/$/, '')}/api/v1/logs/batch`;
    this.token = deviceToken;
    this.flushAtSize = flushAtSize;
    this.maxBuffered = maxBuffered;
    this.buffer = [];
    this.timer = setInterval(() => this.flush(), flushEveryMs);
  }

  trace(tag, msg) { this.log(0, tag, msg); }
  debug(tag, msg) { this.log(1, tag, msg); }
  info(tag, msg)  { this.log(2, tag, msg); }
  warn(tag, msg)  { this.log(3, tag, msg); }
  error(tag, msg, err) {
    this.log(4, tag, err ? `${msg}\n${err.stack || err}` : msg);
  }

  log(level, name, message) {
    this.buffer.push({ name, message: String(message), level, ts: Date.now() });
    // Drop the oldest rather than grow without bound while offline.
    if (this.buffer.length > this.maxBuffered) {
      this.buffer.splice(0, this.buffer.length - this.maxBuffered);
    }
    if (this.buffer.length >= this.flushAtSize) this.flush();
  }

  async flush() {
    if (this.buffer.length === 0) return;
    const batch = this.buffer.splice(0, 500);
    try {
      await fetch(this.endpoint, {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${this.token}`,
          'content-type': 'application/json',
        },
        body: JSON.stringify(batch),
      });
    } catch {
      // Logging must never surface an error into the app.
    }
  }

  stop() { clearInterval(this.timer); return this.flush(); }
}
```

```js
const logger = new RemoteLogger('https://logs.example.com', LOGGER_TOKEN);

logger.info('[Auth] ', `Signed in as ${userId}`);

// Catch what you would otherwise never see from a tester's device.
ErrorUtils.setGlobalHandler((e, isFatal) => {
  logger.error('[Crash] ', isFatal ? 'Fatal' : 'Error', e);
  logger.flush();
});
```

---

## Node.js

Same class as above (Node 18+ has `fetch` built in). A CommonJS variant:

```js
const { setInterval, clearInterval } = require('node:timers');

class RemoteLogger {
  constructor(baseUrl, token, { flushEveryMs = 2000, flushAtSize = 50 } = {}) {
    this.endpoint = `${baseUrl.replace(/\/$/, '')}/api/v1/logs/batch`;
    this.token = token;
    this.flushAtSize = flushAtSize;
    this.buffer = [];
    this.timer = setInterval(() => this.flush(), flushEveryMs);
    this.timer.unref?.();  // do not hold the process open
  }

  log(level, name, message) {
    this.buffer.push({ name, message: String(message), level, ts: Date.now() });
    if (this.buffer.length >= this.flushAtSize) this.flush();
  }
  info(tag, m)  { this.log(2, tag, m); }
  warn(tag, m)  { this.log(3, tag, m); }
  error(tag, m) { this.log(4, tag, m); }

  async flush() {
    if (!this.buffer.length) return;
    const batch = this.buffer.splice(0, 500);
    try {
      await fetch(this.endpoint, {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${this.token}`,
          'content-type': 'application/json',
        },
        body: JSON.stringify(batch),
      });
    } catch {}
  }

  async stop() { clearInterval(this.timer); await this.flush(); }
}

module.exports = { RemoteLogger };
```

---

## Python

Standard library only.

```python
import atexit, json, threading, time, urllib.error, urllib.request


class RemoteLogger:
    def __init__(self, base_url, device_token,
                 flush_every=2.0, flush_at_size=50, max_buffered=1000):
        self.endpoint = base_url.rstrip("/") + "/api/v1/logs/batch"
        self.token = device_token
        self.flush_at_size = flush_at_size
        self.max_buffered = max_buffered
        self._buffer = []
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, args=(flush_every,), daemon=True)
        self._thread.start()
        atexit.register(self.stop)

    def trace(self, tag, message): self.log(0, tag, message)
    def debug(self, tag, message): self.log(1, tag, message)
    def info(self, tag, message):  self.log(2, tag, message)
    def warn(self, tag, message):  self.log(3, tag, message)
    def error(self, tag, message): self.log(4, tag, message)

    def log(self, level, name, message):
        entry = {"name": name, "message": str(message),
                 "level": level, "ts": int(time.time() * 1000)}
        with self._lock:
            self._buffer.append(entry)
            # Drop the oldest rather than grow without bound while offline.
            if len(self._buffer) > self.max_buffered:
                del self._buffer[: len(self._buffer) - self.max_buffered]
            ready = len(self._buffer) >= self.flush_at_size
        if ready:
            self.flush()

    def flush(self):
        with self._lock:
            batch, self._buffer = self._buffer[:500], self._buffer[500:]
        if not batch:
            return
        request = urllib.request.Request(
            self.endpoint,
            data=json.dumps(batch).encode(),
            headers={"Authorization": f"Bearer {self.token}",
                     "content-type": "application/json"},
            method="POST",
        )
        try:
            urllib.request.urlopen(request, timeout=10).close()
        except (urllib.error.URLError, OSError):
            pass  # logging must never raise into the caller

    def _run(self, interval):
        while not self._stop.wait(interval):
            self.flush()

    def stop(self):
        self._stop.set()
        self.flush()
```

```python
logger = RemoteLogger("https://logs.example.com", os.environ["LOGGER_TOKEN"])
logger.info("[worker] ", "Started")
logger.error("[worker] ", "Job 42 failed")
```

### Bridging Python's `logging`

```python
import logging

LEVELS = {logging.DEBUG: 1, logging.INFO: 2, logging.WARNING: 3,
          logging.ERROR: 4, logging.CRITICAL: 4}


class RemoteHandler(logging.Handler):
    def __init__(self, remote):
        super().__init__()
        self.remote = remote

    def emit(self, record):
        self.remote.log(LEVELS.get(record.levelno, 2),
                        f"[{record.name}] ", self.format(record))


logging.getLogger().addHandler(RemoteHandler(logger))
```

---

## Go

Standard library only.

```go
package remotelogger

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"sync"
	"time"
)

type entry struct {
	Name    string `json:"name"`
	Message string `json:"message"`
	Level   int    `json:"level"`
	TS      int64  `json:"ts"`
}

type Logger struct {
	endpoint string
	token    string
	client   *http.Client

	mu     sync.Mutex
	buffer []entry

	flushAt     int
	maxBuffered int
	stop        chan struct{}
	wg          sync.WaitGroup
}

// New starts the background flusher. Call Close when you are done.
func New(baseURL, deviceToken string) *Logger {
	l := &Logger{
		endpoint:    strings.TrimSuffix(baseURL, "/") + "/api/v1/logs/batch",
		token:       deviceToken,
		client:      &http.Client{Timeout: 10 * time.Second},
		flushAt:     50,
		maxBuffered: 1000,
		stop:        make(chan struct{}),
	}
	l.wg.Add(1)
	go l.loop(2 * time.Second)
	return l
}

func (l *Logger) Trace(tag, msg string) { l.Log(0, tag, msg) }
func (l *Logger) Debug(tag, msg string) { l.Log(1, tag, msg) }
func (l *Logger) Info(tag, msg string)  { l.Log(2, tag, msg) }
func (l *Logger) Warn(tag, msg string)  { l.Log(3, tag, msg) }
func (l *Logger) Error(tag, msg string) { l.Log(4, tag, msg) }

func (l *Logger) Log(level int, tag, msg string) {
	l.mu.Lock()
	l.buffer = append(l.buffer, entry{tag, msg, level, time.Now().UnixMilli()})
	// Drop the oldest rather than grow without bound while offline.
	if over := len(l.buffer) - l.maxBuffered; over > 0 {
		l.buffer = l.buffer[over:]
	}
	ready := len(l.buffer) >= l.flushAt
	l.mu.Unlock()
	if ready {
		l.Flush()
	}
}

func (l *Logger) Flush() {
	l.mu.Lock()
	n := len(l.buffer)
	if n > 500 {
		n = 500
	}
	if n == 0 {
		l.mu.Unlock()
		return
	}
	batch := make([]entry, n)
	copy(batch, l.buffer[:n])
	l.buffer = l.buffer[n:]
	l.mu.Unlock()

	body, err := json.Marshal(batch)
	if err != nil {
		return
	}
	req, err := http.NewRequestWithContext(
		context.Background(), http.MethodPost, l.endpoint, bytes.NewReader(body))
	if err != nil {
		return
	}
	req.Header.Set("Authorization", "Bearer "+l.token)
	req.Header.Set("Content-Type", "application/json")

	// Errors are deliberately swallowed: logging must not disrupt the caller.
	resp, err := l.client.Do(req)
	if err == nil {
		resp.Body.Close()
	}
}

func (l *Logger) loop(every time.Duration) {
	defer l.wg.Done()
	t := time.NewTicker(every)
	defer t.Stop()
	for {
		select {
		case <-t.C:
			l.Flush()
		case <-l.stop:
			return
		}
	}
}

// Close stops the flusher and sends whatever is still buffered.
func (l *Logger) Close() {
	close(l.stop)
	l.wg.Wait()
	l.Flush()
}
```

```go
lg := remotelogger.New("https://logs.example.com", os.Getenv("LOGGER_TOKEN"))
defer lg.Close()

lg.Info("[api] ", "listening on :8080")
lg.Error("[db] ", fmt.Sprintf("query failed: %v", err))
```

### Bridging `log/slog`

```go
type SlogHandler struct {
	slog.Handler
	remote *remotelogger.Logger
}

func (h SlogHandler) Handle(ctx context.Context, r slog.Record) error {
	level := map[slog.Level]int{
		slog.LevelDebug: 1, slog.LevelInfo: 2,
		slog.LevelWarn: 3, slog.LevelError: 4,
	}[r.Level]
	h.remote.Log(level, "[app] ", r.Message)
	return h.Handler.Handle(ctx, r)
}
```

---

## PHP

No dependencies; `curl` is enough. Because a typical PHP request is short-lived
there is no background thread — the buffer is flushed when the script ends,
registered through `register_shutdown_function`.

```php
<?php

final class RemoteLogger
{
    private string $endpoint;
    private string $token;
    private array $buffer = [];
    private int $flushAtSize;
    private int $maxBuffered;

    public function __construct(
        string $baseUrl,
        string $deviceToken,
        int $flushAtSize = 50,
        int $maxBuffered = 1000
    ) {
        $this->endpoint = rtrim($baseUrl, '/') . '/api/v1/logs/batch';
        $this->token = $deviceToken;
        $this->flushAtSize = $flushAtSize;
        $this->maxBuffered = $maxBuffered;
        // A PHP request ends quickly, so flush on shutdown rather than on a timer.
        register_shutdown_function([$this, 'flush']);
    }

    public function trace(string $tag, string $m): void { $this->log(0, $tag, $m); }
    public function debug(string $tag, string $m): void { $this->log(1, $tag, $m); }
    public function info(string $tag, string $m): void  { $this->log(2, $tag, $m); }
    public function warn(string $tag, string $m): void  { $this->log(3, $tag, $m); }
    public function error(string $tag, string $m): void { $this->log(4, $tag, $m); }

    public function log(int $level, string $name, string $message): void
    {
        $this->buffer[] = [
            'name' => $name,
            'message' => $message,
            'level' => $level,
            'ts' => (int) round(microtime(true) * 1000),
        ];
        // Drop the oldest rather than grow without bound.
        if (count($this->buffer) > $this->maxBuffered) {
            $this->buffer = array_slice($this->buffer, -$this->maxBuffered);
        }
        if (count($this->buffer) >= $this->flushAtSize) {
            $this->flush();
        }
    }

    public function flush(): void
    {
        if ($this->buffer === []) {
            return;
        }
        $batch = array_splice($this->buffer, 0, 500);

        $ch = curl_init($this->endpoint);
        curl_setopt_array($ch, [
            CURLOPT_POST => true,
            CURLOPT_POSTFIELDS => json_encode($batch),
            CURLOPT_HTTPHEADER => [
                'Authorization: Bearer ' . $this->token,
                'Content-Type: application/json',
            ],
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_TIMEOUT => 10,
        ]);
        // Failures are ignored on purpose: logging must not break the response.
        curl_exec($ch);
        curl_close($ch);
    }
}
```

```php
$logger = new RemoteLogger('https://logs.example.com', getenv('LOGGER_TOKEN'));

$logger->info('[web] ', 'Rendered /checkout in 42ms');

set_exception_handler(function (Throwable $e) use ($logger) {
    $logger->error('[php] ', $e->getMessage() . "\n" . $e->getTraceAsString());
    $logger->flush();
});
```

> On PHP-FPM, prefer `fastcgi_finish_request()` before the flush so the user is
> not kept waiting on your log upload.

---

## C

Uses libcurl. Build with `cc logger.c -lcurl`.

This one is intentionally the simplest thing that works — a fixed-capacity
buffer, no threads. Call `rlog_flush()` yourself at a sensible point, or every
few seconds from whatever loop your program already has.

```c
#include <curl/curl.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>

#define RLOG_MAX_ENTRIES 256
#define RLOG_MAX_MSG 1024

typedef struct {
    char url[512];
    char auth[256];
    char json[RLOG_MAX_ENTRIES * (RLOG_MAX_MSG + 128)];
    size_t json_len;
    int count;
} rlog_t;

static rlog_t rlog;

void rlog_flush(void);   /* rlog_log calls this when the buffer fills */

/* Discards the response body; a logger should not write to stdout. */
static size_t rlog_sink(void *p, size_t sz, size_t n, void *u) {
    (void) p; (void) u;
    return sz * n;
}

void rlog_init(const char *base_url, const char *token) {
    curl_global_init(CURL_GLOBAL_DEFAULT);
    snprintf(rlog.url, sizeof rlog.url, "%s/api/v1/logs/batch", base_url);
    snprintf(rlog.auth, sizeof rlog.auth, "Authorization: Bearer %s", token);
    rlog.json_len = 0;
    rlog.count = 0;
}

/* Escapes the characters JSON forbids in a string. */
static void json_escape(const char *in, char *out, size_t cap) {
    size_t o = 0;
    for (size_t i = 0; in[i] && o + 7 < cap; i++) {
        unsigned char c = (unsigned char) in[i];
        switch (c) {
            case '"':  out[o++] = '\\'; out[o++] = '"';  break;
            case '\\': out[o++] = '\\'; out[o++] = '\\'; break;
            case '\n': out[o++] = '\\'; out[o++] = 'n';  break;
            case '\r': out[o++] = '\\'; out[o++] = 'r';  break;
            case '\t': out[o++] = '\\'; out[o++] = 't';  break;
            default:
                if (c < 0x20) { o += (size_t) snprintf(out + o, cap - o, "\\u%04x", c); }
                else          { out[o++] = (char) c; }
        }
    }
    out[o] = '\0';
}

static long long rlog_now_ms(void) {
    struct timeval tv;
    gettimeofday(&tv, NULL);
    return (long long) tv.tv_sec * 1000 + tv.tv_usec / 1000;
}

void rlog_log(int level, const char *tag, const char *message) {
    if (rlog.count >= RLOG_MAX_ENTRIES) rlog_flush();

    char etag[256], emsg[RLOG_MAX_MSG * 6 + 1];
    json_escape(tag, etag, sizeof etag);
    json_escape(message, emsg, sizeof emsg);

    int n = snprintf(rlog.json + rlog.json_len,
                     sizeof rlog.json - rlog.json_len,
                     "%s{\"name\":\"%s\",\"message\":\"%s\",\"level\":%d,\"ts\":%lld}",
                     rlog.count ? "," : "", etag, emsg, level, rlog_now_ms());
    if (n <= 0 || (size_t) n >= sizeof rlog.json - rlog.json_len) {
        rlog_flush();          /* buffer full: send what we have and drop this line */
        return;
    }
    rlog.json_len += (size_t) n;
    rlog.count++;
}

void rlog_flush(void) {
    if (rlog.count == 0) return;

    char body[sizeof rlog.json + 2];
    snprintf(body, sizeof body, "[%s]", rlog.json);
    rlog.json_len = 0;
    rlog.count = 0;

    CURL *curl = curl_easy_init();
    if (!curl) return;

    struct curl_slist *headers = NULL;
    headers = curl_slist_append(headers, rlog.auth);
    headers = curl_slist_append(headers, "Content-Type: application/json");

    curl_easy_setopt(curl, CURLOPT_URL, rlog.url);
    curl_easy_setopt(curl, CURLOPT_POSTFIELDS, body);
    curl_easy_setopt(curl, CURLOPT_HTTPHEADER, headers);
    curl_easy_setopt(curl, CURLOPT_TIMEOUT, 10L);
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, rlog_sink);

    curl_easy_perform(curl);   /* result ignored: logging must not fail the program */

    curl_slist_free_all(headers);
    curl_easy_cleanup(curl);
}

void rlog_shutdown(void) {
    rlog_flush();
    curl_global_cleanup();
}
```

```c
int main(void) {
    rlog_init("https://logs.example.com", getenv("LOGGER_TOKEN"));

    rlog_log(2, "[main] ", "started");
    rlog_log(4, "[main] ", "something went wrong");

    rlog_shutdown();
    return 0;
}
```

> Not thread-safe as written. If you log from more than one thread, guard
> `rlog_log` and `rlog_flush` with a mutex.

---

## C++

C++17, libcurl, with a background flush thread. Build with
`c++ -std=c++17 logger.cpp -lcurl -lpthread`.

```cpp
#include <curl/curl.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <mutex>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

class RemoteLogger {
public:
    RemoteLogger(std::string base_url, std::string token,
                 std::chrono::milliseconds flush_every = std::chrono::seconds(2),
                 std::size_t flush_at = 50, std::size_t max_buffered = 1000)
        : endpoint_(std::move(base_url) + "/api/v1/logs/batch"),
          auth_("Authorization: Bearer " + std::move(token)),
          flush_at_(flush_at), max_buffered_(max_buffered) {
        curl_global_init(CURL_GLOBAL_DEFAULT);
        worker_ = std::thread([this, flush_every] {
            std::unique_lock<std::mutex> lock(stop_mutex_);
            while (!stop_.load()) {
                stop_cv_.wait_for(lock, flush_every);
                flush();
            }
        });
    }

    ~RemoteLogger() {
        stop_.store(true);
        stop_cv_.notify_all();
        if (worker_.joinable()) worker_.join();
        flush();
        curl_global_cleanup();
    }

    RemoteLogger(const RemoteLogger&) = delete;
    RemoteLogger& operator=(const RemoteLogger&) = delete;

    void trace(const std::string& t, const std::string& m) { log(0, t, m); }
    void debug(const std::string& t, const std::string& m) { log(1, t, m); }
    void info (const std::string& t, const std::string& m) { log(2, t, m); }
    void warn (const std::string& t, const std::string& m) { log(3, t, m); }
    void error(const std::string& t, const std::string& m) { log(4, t, m); }

    void log(int level, const std::string& tag, const std::string& message) {
        std::ostringstream entry;
        entry << R"({"name":")" << escape(tag)
              << R"(","message":")" << escape(message)
              << R"(","level":)" << level
              << R"(,"ts":)" << now_ms() << '}';

        bool ready;
        {
            std::lock_guard<std::mutex> lock(mutex_);
            buffer_.push_back(entry.str());
            // Drop the oldest rather than grow without bound while offline.
            if (buffer_.size() > max_buffered_) {
                buffer_.erase(buffer_.begin(),
                              buffer_.begin() + (buffer_.size() - max_buffered_));
            }
            ready = buffer_.size() >= flush_at_;
        }
        if (ready) flush();
    }

    void flush() {
        std::vector<std::string> batch;
        {
            std::lock_guard<std::mutex> lock(mutex_);
            if (buffer_.empty()) return;
            const std::size_t n = std::min<std::size_t>(buffer_.size(), 500);
            batch.assign(buffer_.begin(), buffer_.begin() + n);
            buffer_.erase(buffer_.begin(), buffer_.begin() + n);
        }

        std::string body = "[";
        for (std::size_t i = 0; i < batch.size(); ++i) {
            if (i) body += ',';
            body += batch[i];
        }
        body += ']';

        CURL* curl = curl_easy_init();
        if (!curl) return;

        curl_slist* headers = nullptr;
        headers = curl_slist_append(headers, auth_.c_str());
        headers = curl_slist_append(headers, "Content-Type: application/json");

        curl_easy_setopt(curl, CURLOPT_URL, endpoint_.c_str());
        curl_easy_setopt(curl, CURLOPT_POSTFIELDS, body.c_str());
        curl_easy_setopt(curl, CURLOPT_POSTFIELDSIZE, (long) body.size());
        curl_easy_setopt(curl, CURLOPT_HTTPHEADER, headers);
        curl_easy_setopt(curl, CURLOPT_TIMEOUT, 10L);
        // Discard the response; a logger should not write to stdout.
        curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, &RemoteLogger::sink);

        curl_easy_perform(curl);  // ignored: logging must not disrupt the program

        curl_slist_free_all(headers);
        curl_easy_cleanup(curl);
    }

private:
    static std::size_t sink(void*, std::size_t size, std::size_t n, void*) {
        return size * n;
    }

    static long long now_ms() {
        using namespace std::chrono;
        return duration_cast<milliseconds>(system_clock::now().time_since_epoch()).count();
    }

    static std::string escape(const std::string& in) {
        std::string out;
        out.reserve(in.size() + 16);
        for (unsigned char c : in) {
            switch (c) {
                case '"':  out += "\\\""; break;
                case '\\': out += "\\\\"; break;
                case '\n': out += "\\n";  break;
                case '\r': out += "\\r";  break;
                case '\t': out += "\\t";  break;
                default:
                    if (c < 0x20) {
                        char buf[8];
                        std::snprintf(buf, sizeof buf, "\\u%04x", c);
                        out += buf;
                    } else {
                        out += static_cast<char>(c);
                    }
            }
        }
        return out;
    }

    std::string endpoint_, auth_;
    std::size_t flush_at_, max_buffered_;

    std::mutex mutex_;
    std::vector<std::string> buffer_;

    std::thread worker_;
    std::atomic<bool> stop_{false};
    std::mutex stop_mutex_;
    std::condition_variable stop_cv_;
};
```

```cpp
int main() {
    RemoteLogger logger("https://logs.example.com", std::getenv("LOGGER_TOKEN"));

    logger.info("[engine] ", "frame budget 16.6ms");
    logger.error("[engine] ", "shader compile failed");
    // Destructor stops the worker and flushes what is left.
}
```

---

## curl

For a shell script or a CI job:

```sh
# One line.
curl -X POST "$LOGGER_URL/api/v1/logs" \
  -H "Authorization: Bearer $LOGGER_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"[ci] ","message":"build 412 passed","level":2}'

# Several at once.
curl -X POST "$LOGGER_URL/api/v1/logs/batch" \
  -H "Authorization: Bearer $LOGGER_TOKEN" \
  -H 'content-type: application/json' \
  -d '[{"name":"[ci] ","message":"step 1"},{"name":"[ci] ","message":"step 2"}]'
```

Piping an existing log file in, one line per record:

```sh
tail -f /var/log/myapp.log | while IFS= read -r line; do
  curl -s -o /dev/null -X POST "$LOGGER_URL/api/v1/logs" \
    -H "Authorization: Bearer $LOGGER_TOKEN" \
    -H 'content-type: application/json' \
    --data-raw "$(jq -nc --arg m "$line" '{name:"[syslog] ",message:$m}')"
done
```

---

## Field reference

| Field | Required | Notes |
|---|---|---|
| `message` | yes | The log text. Truncated at 50,384 characters. |
| `name` | no | Free-text tag, e.g. `"[Network] "`. Max 255 characters. |
| `level` | no | `0`–`4`. Defaults to `2` (info); anything higher is clamped to `4`. |
| `ts` | no | Unix **milliseconds**. Defaults to server receipt time. |
| `context` | no | JSON **object** of structured fields. Max 8 KB. |

Send `context` when you have anything worth correlating on later — a session
id, the app version, the signed-in user:

```json
{ "name": "[Net] ", "message": "checkout failed",
  "context": { "session": "s-42", "appVersion": "3.1.0", "userId": "u_88213" } }
```

It costs nothing to send and turns "an error happened" into "this error only
happens on 3.1.0, for users in this session". It is also what makes
[an AI agent](agents.md) able to answer questions about your logs.

Send `ts` from the device if you care about the moment something happened rather
than the moment it was uploaded — it matters once you are batching, and much
more if the app buffered while offline.

`device_id` and `device` are attached by the server from your token. Sending
them does nothing; you cannot log as another device.

## What comes back

`POST /api/v1/logs` returns `202` and `{"id":1,"ts":1788067277526}` as soon as
the line is queued and broadcast to any open dashboard. It is written to disk a
few milliseconds later. Add `?sync=true` to wait for the write and get `201`
instead — useful in a test, wasteful in an app.

`POST /api/v1/logs/batch` returns `{"accepted":50,"dropped":0,"first_id":1,"last_id":50}`.
**Check `dropped`.** If the server's write queue is full it accepts what it can
and tells you how many it refused; those are the last `dropped` entries of your
batch, and resending them is up to you.

Full detail in the [API reference](api.md).
