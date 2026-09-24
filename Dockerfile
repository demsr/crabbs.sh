# syntax=docker/dockerfile:1
#
# Production image for rust-bbs. Two stages: compile with the full Rust
# toolchain, then ship just the two resulting binaries on a minimal base.
# Nothing here needs libsqlite3 or OpenSSL at runtime - rusqlite's "bundled"
# feature compiles SQLite in, and russh's crypto backend (aws-lc-rs) is
# self-contained - so the runtime stage carries no C libraries at all.

########## build ##########
FROM rust:1-slim-bookworm AS builder

# build-essential: C compiler for rusqlite's bundled SQLite and for russh's
# crypto backend. cmake/perl: aws-lc-sys' fallback build path, only used if
# it can't find prebuilt bindings for the target - unneeded on the common
# x86_64/aarch64 glibc targets, but cheap insurance since this stage is
# discarded and never affects the shipped image.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake perl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Compile dependencies in their own layer, so editing src/ later doesn't
# force recompiling the whole dependency tree (argon2, russh, ratatui, ...)
# on every build - only Cargo.toml/Cargo.lock changing does.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > src/bin/bbsadmin.rs \
    && : > src/lib.rs \
    && cargo build --release --locked --bins \
    && rm -rf src

# Now the real source, replacing the stubs above. `touch` guarantees cargo
# sees it as changed even if COPY happens to preserve an mtime that isn't
# newer than the stub build's.
COPY src ./src
RUN touch src/main.rs src/lib.rs src/bin/bbsadmin.rs \
    && cargo build --release --locked --bins

########## runtime ##########
FROM debian:bookworm-slim AS runtime

# Unprivileged account; /data is both its home and the volume mount point
# (matches BBS_DATA_DIR below), so ownership only needs setting once.
RUN groupadd --system bbs \
    && useradd --system --gid bbs --home-dir /data --shell /usr/sbin/nologin bbs \
    && mkdir -p /data \
    && chown bbs:bbs /data

COPY --from=builder /build/target/release/rust-bbs /usr/local/bin/rust-bbs
COPY --from=builder /build/target/release/bbsadmin /usr/local/bin/bbsadmin

USER bbs
WORKDIR /data
ENV BBS_DATA_DIR=/data
# BBS_GUEST_PASSWORD is deliberately not set: the guest account (and with it
# self-registration) stays disabled until you pass one in. See README.
EXPOSE 2222
VOLUME ["/data"]

# Confirms the SSH port is accepting connections; doesn't perform a full SSH
# handshake, just enough to catch a hung or crashed process for `docker run
# --restart`. Needs bash for /dev/tcp (debian-slim's /bin/sh is dash, which
# lacks it); bash is part of Debian's required set, so no extra install.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/${BBS_PORT:-2222}'

ENTRYPOINT ["rust-bbs"]
