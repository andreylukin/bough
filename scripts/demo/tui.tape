# The README hero: bough fixes failing tests in the demo repo.
# Run from the repo root:  vhs scripts/demo/tui.tape
# Uses a real model (see setup.sh) on a fresh sandbox.
#
# Every Type waits for the screen it needs first: typing ahead lands
# keystrokes in the wrong program.

Output assets/demo.gif

Require bough
Set Shell zsh
Set FontSize 15
Set Width 1280
Set Height 760
Set Padding 16
Set TypingSpeed 35ms
Set Theme "Catppuccin Mocha"

Hide
Type `export PATH="${DEMO_BIN:+$DEMO_BIN:}$PATH" && d=$(mktemp -d) && scripts/demo/setup.sh $d >/dev/null && export HOME=$d/home && cd $d/wordfreq && PS1='$ ' && clear`
Enter
Wait /^\$/
Show

Type "go test ./... 2>&1 | grep -E '^(--- FAIL|FAIL|ok)'"
Enter
Wait+Screen@30s /FAIL\s+example/
Sleep 2s

Type "bough"
Enter
Wait+Screen@30s /say something/
Sleep 1s

Type "go test fails. Fix TopN so ties sort alphabetically and a big n doesn't panic, then run the tests."
Sleep 500ms
Enter

# The done chip names the file, often by absolute path.
Wait+Screen@300s /wrote .*wordfreq\.go/
Sleep 5s

Ctrl+C
Sleep 300ms
Ctrl+C
Wait /^\$/

Hide
Type "clear"
Enter
Show
# -count=1: an "(cached)" pass reads as if nothing ran again.
Type "go test -count=1 ./..."
Enter
Wait+Screen@30s /ok\s+example/
Sleep 3s
