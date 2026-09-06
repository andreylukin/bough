#!/bin/sh
# Start bough's local memory model (memoryd) if it is not already up.
# Model: BOUGH_MEMORY_MODEL (default Granite 4.0 H-Small 4-bit), port 8765.
cd "$(dirname "$0")" || exit 1
curl -sf localhost:${BOUGH_MEMORY_PORT:-8765}/status >/dev/null 2>&1 && { echo "memoryd already running"; exit 0; }
. venv/bin/activate
nohup python memoryd.py >> memoryd.log 2>&1 &
echo "memoryd starting (pid $!); log: $(pwd)/memoryd.log"
