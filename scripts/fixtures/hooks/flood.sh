#!/bin/sh
# Exits zero and writes far more than any sane `max_output_bytes`.
cat > /dev/null
i=0
while [ $i -lt 200 ]; do printf '%0100d' 0; i=$((i+1)); done
