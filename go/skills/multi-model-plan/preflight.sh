#!/bin/sh
# preflight.sh <plugin/model>... — check each model answers a trivial
# headless prompt. Prints the working models; exits 1 if fewer than 3
# distinct ones work.
bin="${BOUGH_BIN:-bough}"
if [ $# -eq 0 ]; then
	echo "usage: preflight.sh <plugin/model>..." >&2
	exit 2
fi
# macOS has no timeout(1) unless coreutils is installed; run unbounded then.
to=""
if command -v timeout >/dev/null 2>&1; then
	to="timeout 120"
elif command -v gtimeout >/dev/null 2>&1; then
	to="gtimeout 120"
fi
ok=""
for pm in "$@"; do
	p="${pm%%/*}"
	m="${pm#*/}"
	if [ "$p" = "$pm" ] || [ -z "$m" ]; then
		echo "FAIL $pm (want plugin/model)" >&2
		continue
	fi
	out=$(printf 'Reply with exactly: OK' |
		BOUGH_WEB_ADDR=127.0.0.1:0 $to "$bin" --headless --set llm.plugin="$p" --set llm.model="$m" 2>&1)
	code=$?
	if [ $code -eq 0 ] && printf '%s' "$out" | grep -q 'OK'; then
		case " $ok " in *" $pm "*) ;; *) ok="$ok $pm" ;; esac
	else
		echo "FAIL $pm (exit $code): $(printf '%s' "$out" | tail -n 1)" >&2
	fi
done
n=0
for pm in $ok; do
	echo "$pm"
	n=$((n + 1))
done
if [ $n -lt 3 ]; then
	echo "preflight: only $n distinct working model(s); need 3" >&2
	exit 1
fi
