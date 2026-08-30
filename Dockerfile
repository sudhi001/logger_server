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

FROM scratch
# WORKDIR creates the directory; a scratch image has no shell to mkdir with.
WORKDIR /data
COPY --from=builder /build/target/release/logger_server /logger_server
ENV LOGGER_DB_PATH=/data/logs.db
EXPOSE 8080
ENTRYPOINT ["/logger_server"]
