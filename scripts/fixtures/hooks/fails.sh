#!/bin/sh
# Fails every time, loudly. The quarantine fixture.
cat > /dev/null
[ -n "$HOOK_RECORD" ] && printf 'ran\n' >> "$HOOK_RECORD"
echo "this hook is broken" >&2
exit 3
