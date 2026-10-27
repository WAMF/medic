#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
cargo build --release --locked --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl
mkdir -p dist
cp target/x86_64-unknown-linux-musl/release/medic dist/medic-linux-amd64
cp target/aarch64-unknown-linux-musl/release/medic dist/medic-linux-arm64
(cd dist && sha256sum medic-linux-amd64 medic-linux-arm64 > SHA256SUMS && sha256sum -c SHA256SUMS)
./dist/medic-linux-amd64 --version
./dist/medic-linux-amd64 config --defaults > /dev/null
file dist/medic-linux-arm64 | grep -q 'ARM aarch64'
