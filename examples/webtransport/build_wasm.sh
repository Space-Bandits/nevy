#!/bin/bash
set -e

echo "Building WASM..."
cd "$(dirname "$0")"

# Build for wasm32 with web features
RUSTFLAGS='--cfg=web_sys_unstable_apis' cargo build \
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
echo "Build complete! To test:"
echo "  1. Start the native server: cargo run --bin server"
echo "  2. Serve the web directory: python3 -m http.server 8080 -d web"
echo "  3. Open http://localhost:8080 in Chrome"
echo "  4. Paste the certificate hash from the server output and click Connect"
