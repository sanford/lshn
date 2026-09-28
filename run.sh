#!/usr/bin/env bash
# Build lshn, install it to ~/.local/bin, and run it.
# Any arguments are passed through: ./run.sh best, ./run.sh 12345 | less -R, ...
set -euo pipefail

cd "$(dirname "$0")"
bin_dir="$HOME/.local/bin"

cargo build --release --quiet
mkdir -p "$bin_dir"
install -m 755 target/release/lshn "$bin_dir/lshn"

exec "$bin_dir/lshn" "$@"
