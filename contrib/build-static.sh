#!/usr/bin/env bash
# Builds a static x86_64 Linux bullnym binary, with the web app embedded, in
# a Debian Rust container with musl. The binary runs on any x86_64 Linux.
# The compiler runs at the lowest CPU priority, so the build does not slow
# a shared machine down.
#
# Environment:
#   OUT_DIR           where the binary goes (default: target/static)
#   CARGO_TARGET_DIR  the cargo target directory (default: target)
#   CARGO_HOME        the cargo home to reuse for downloads (default: ~/.cargo)
#   RUST_IMAGE        the build image (default: rust:1-bookworm)
#   CARGO_BUILD_JOBS  parallel jobs (default: 8)
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT_DIR:-$repo/target/static}
target_dir=${CARGO_TARGET_DIR:-$repo/target}
cargo_home=${CARGO_HOME:-$HOME/.cargo}
image=${RUST_IMAGE:-rust:1-bookworm}
jobs=${CARGO_BUILD_JOBS:-8}

if [ ! -f "$repo/web/dist/index.html" ]; then
	(cd "$repo/web" && npm ci && npm run build)
fi
mkdir -p "$out" "$target_dir"

docker run --rm \
	-v "$repo:/src:ro" -v "$target_dir:/target" -v "$cargo_home:/cargo" \
	-e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target -e CARGO_BUILD_JOBS="$jobs" \
	-e BULLNYM_WEB_DIST=/src/web/dist \
	-e CC_x86_64_unknown_linux_musl=musl-gcc \
	-w /src "$image" sh -ec '
		apt-get update -qq
		apt-get install -y -qq musl-tools protobuf-compiler cmake >/dev/null
		rustup target add x86_64-unknown-linux-musl
		nice -n 19 cargo build --locked --release --target x86_64-unknown-linux-musl --bin bullnym
		chown -R '"$(id -u):$(id -g)"' /target /cargo
	'

install -m 0755 "$target_dir/x86_64-unknown-linux-musl/release/bullnym" "$out/bullnym"
file "$out/bullnym"
sha256sum "$out/bullnym"
