#!/bin/sh
# Builds Acorn's web window into web/dist: the app as WebAssembly, nsh's
# page runtime (nsh.js, from the nsh-platform-web crate) and the page.
# acorn-server serves it.
set -eu
cd "$(dirname "$0")/.."
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo build --release --target wasm32-unknown-unknown -p acorn-web
nsh=$(cargo metadata --format-version 1 --filter-platform wasm32-unknown-unknown |
  python3 -c 'import json,sys,os; m=json.load(sys.stdin); print(next(os.path.dirname(p["manifest_path"]) for p in m["packages"] if p["name"]=="nsh-platform-web"))')
mkdir -p web/dist
cp target/wasm32-unknown-unknown/release/acorn_web.wasm web/dist/acorn.wasm
if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int web/dist/acorn.wasm -o web/dist/acorn.wasm
fi
cp "$nsh/web/nsh.js" web/dist/nsh.js
cp web/index.html web/dist/index.html
echo "web/dist: $(du -h web/dist/acorn.wasm | cut -f1) wasm"
