#!/usr/bin/env bash
# Build lshn, install it to ~/.local/bin, and run it.
# Any arguments are passed through: ./run.sh best, ./run.sh 12345 | less -R, ...
set -euo pipefail

cd "$(dirname "$0")"
bin_dir="$HOME/.local/bin"

# Not quiet: after a change the build takes a few seconds, and cargo's
# progress shows that's what the wait is.
cargo build --release
mkdir -p "$bin_dir"
install -m 755 target/release/lshn "$bin_dir/lshn"

exec "$bin_dir/lshn" "$@"
