#!/usr/bin/env bash
set -euo pipefail

if [[ $# -gt 1 ]]; then
    printf 'usage: %s [OUTPUT_BASENAME]\n' "$0" >&2
    exit 2
fi

output_name=${1:-og-param}
if [[ "$output_name" == */* || -z "$output_name" ]]; then
    printf 'output basename must not contain a path separator: %s\n' "$output_name" >&2
    exit 2
fi

linux_executable="/output/$output_name-linux-x86_64"
windows_executable="/output/$output_name-windows-x86_64.exe"

touch src/*.rs
cargo build --locked --release --target x86_64-unknown-linux-musl
cargo build --locked --release --target x86_64-pc-windows-gnu

install -m755 target/x86_64-unknown-linux-musl/release/og-param "$linux_executable"
install -m755 target/x86_64-pc-windows-gnu/release/og-param.exe "$windows_executable"
strip "$linux_executable"
x86_64-w64-mingw32-strip "$windows_executable"

if [[ -n ${OG_PARAM_OUTPUT_UID:-} && -n ${OG_PARAM_OUTPUT_GID:-} ]]; then
    chown "$OG_PARAM_OUTPUT_UID:$OG_PARAM_OUTPUT_GID" \
        "$linux_executable" "$windows_executable"
fi

printf 'Artifacts written:\n  %s\n  %s\n' "$linux_executable" "$windows_executable"
