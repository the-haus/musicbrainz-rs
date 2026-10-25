#!/bin/sh
# Build and test the Linux binary for one napi target inside a Node image of
# the runner's own architecture, so the binary runs where it was built (and
# DuckDB and zstd build with the image's own compiler):
#
#   docker run --rm -v "$PWD:/w" -w /w node:22-bullseye \
#     sh crates/bindings/musicbrainz-node/scripts/build-linux.sh aarch64-unknown-linux-gnu
#
# Debian bullseye sets the glibc floor at 2.31; node:22-alpine builds musl.
set -eu
TARGET="$1"

if [ -f /etc/alpine-release ]; then
  apk add --no-cache build-base curl bash cmake linux-headers
  # A Node addon is a cdylib that Node dlopen()s: link musl's libc (and
  # libunwind/libgcc_s) dynamically. Rust's static-CRT default on musl makes
  # the loader fail with "_Unwind_Resume: initial-exec TLS resolves to
  # dynamic definition".
  # RUSTFLAGS replaces .cargo/config.toml's flags: keep its loop alignment.
  export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=-crt-static -C llvm-args=-align-loops=64"
fi
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
# shellcheck source=/dev/null
. "$HOME/.cargo/env"

corepack enable
COREPACK_ENABLE_DOWNLOAD_PROMPT=0 yarn install --immutable
cd crates/bindings/musicbrainz-node
yarn exec napi build --platform --release --strip --js false --target "$TARGET" ${FEATURES:+--features "$FEATURES"}
yarn run build:ts
yarn run test
