# Building and publishing the image

Two Dockerfiles are provided.

| File | Base | Image size | Idle RSS | Use it for |
|---|---|---|---|---|
| `Dockerfile` | `scratch` (static musl) | ~6 MB | ~6–9 MB | Production. Default. |
| `Dockerfile.glibc` | `distroless/cc` | ~25 MB | ~8–15 MB | When you need glibc or native deps. |

Both are multi-stage: the dependency tree is compiled in a cached layer, so
editing `src/` does not rebuild the world.

## Build

```sh
docker build --platform linux/amd64 -t sudhis/logger_server:2.0.0 .
```

The glibc variant:

```sh
docker build --platform linux/amd64 -f Dockerfile.glibc -t sudhis/logger_server:2.0.0-glibc .
```

`--platform linux/amd64` matters when building on Apple Silicon: the deploy
target is amd64, and the release profile uses fat LTO, so expect the emulated
build to take a while. It is cached after the first run.

## Run

```sh
docker run --rm -p 8080:8080 -v logger-data:/data sudhis/logger_server:2.0.0
```

The volume at `/data` is what makes logs survive a restart. Without it the
SQLite file lives in the container's writable layer and disappears with the
container.

## Push

```sh
docker login
docker push sudhis/logger_server:2.0.0
```

## Verify the memory claim

```sh
docker run --rm -d --name lg -p 8080:8080 sudhis/logger_server:2.0.0
docker stats --no-stream lg          # expect well under 10 MB at idle

# Open 500 live tails and measure again.
for i in $(seq 1 500); do curl -N -s localhost:8080/logs/stream > /dev/null & done
docker stats --no-stream lg          # expect under 20 MB
```

`/metrics` exposes `logger_sse_clients`, `logger_sse_evicted_total`, and
`logger_shed_total`, which is the quickest way to confirm the server is holding
its bounds rather than quietly buffering.
