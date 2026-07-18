#!/usr/bin/env bash
# Build the liasse-connect-wasm core into the two wasm-pack packages the shell uses:
#
#   wasm/web   (--target web)     ESM the browser loads; imported by src/wasm.ts.
#   wasm/node  (--target nodejs)  CommonJS the node integration test drives directly.
#
# Both are generated artifacts (see .gitignore) — the single source of truth for the
# §12.2 wire logic is the Rust crate, never a checked-in binary. Re-run after any
# change to crates/liasse-connect-wasm.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
crate="$here/../../crates/liasse-connect-wasm"

wasm-pack build --target web    --out-dir "$here/wasm/web"  "$crate"
wasm-pack build --target nodejs --out-dir "$here/wasm/node" "$crate"

echo "built wasm/web and wasm/node from $crate"
