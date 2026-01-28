#!/bin/bash
set -e

cd "$(dirname "$0")"

echo "Building WASM..."
RUSTFLAGS='--cfg=web_sys_unstable_apis --cfg=getrandom_backend="wasm_js"' cargo build \
    --target wasm32-unknown-unknown \
    --features web \
    --no-default-features \
    --lib

echo "Running wasm-bindgen..."
wasm-bindgen \
    --out-dir web/pkg \
    --target web \
    ../../target/wasm32-unknown-unknown/debug/webtransport_example.wasm

echo ""
echo "Build complete! Files in web/pkg/"
echo ""
echo "To test:"
echo "  1. Run the server: cargo run --bin server --features native"
echo "  2. Serve the web directory: python3 -m http.server 8080 --directory web"
echo "  3. Open http://localhost:8080"
