#!/usr/bin/env bash
# Build secure-note via Docker and copy the binary to ./build/
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

mkdir -p build

echo "Building secure-note (this may take a while on first run)..."
DOCKER_BUILDKIT=1 docker build \
    --target export \
    --output type=local,dest=./build \
    .

# Docker --output preserves the path inside the scratch stage (/secure-note),
# so rename to the expected binary name.
if [ -f "./build/secure-note" ]; then
    chmod +x ./build/secure-note
elif [ -f "./build/secure-note" ]; then
    : # already in place
fi

echo ""
echo "Done → ./build/secure-note"
