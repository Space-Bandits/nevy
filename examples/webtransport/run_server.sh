#!/bin/bash
set -e

cd "$(dirname "$0")"
cargo run --bin server --features native
