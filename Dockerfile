# Default image: fully static musl binary on an empty base.
# Result is ~6 MB with no OS, no shell, and no CVE surface to patch.
FROM --platform=linux/amd64 rust:1.98-alpine AS builder

# musl-dev/gcc are needed to compile rusqlite's bundled SQLite C sources.
RUN apk add --no-cache musl-dev gcc

WORKDIR /build

# Dependencies are cached separately from source, so editing src/ does not
# trigger a full rebuild of the dependency tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release --features mimalloc && \
    rm -rf src

COPY src ./src
COPY static ./static
# touch: cargo keys off mtime, and the COPY above can preserve an older one.
RUN touch src/main.rs && cargo build --release --features mimalloc

# A scratch image has no shell, so the data directory has to be built here with
# the ownership it needs and copied in whole.
RUN mkdir -p /out/data

FROM scratch

# 65532 is the conventional "nonroot" uid, the same one distroless uses. There
# is no /etc/passwd in a scratch image, so it can only be numeric.
#
# The directory is copied rather than created with WORKDIR because a fresh named
# volume inherits its ownership from the image. Getting that wrong means SQLite
# cannot create logs.db-wal beside the database and the server dies on boot.
COPY --from=builder --chown=65532:65532 /out /
COPY --from=builder /build/target/release/logger_server /logger_server

USER 65532:65532
WORKDIR /data
ENV LOGGER_DB_PATH=/data/logs.db
# mimalloc eagerly commits its arenas on startup, which costs ~7 MB of resident
# memory the server never uses. Measured: 12.6 MB -> 5.8 MB anonymous RSS.
ENV MIMALLOC_ARENA_EAGER_COMMIT=0
EXPOSE 8080
ENTRYPOINT ["/logger_server"]
