# VHS tape: capture the bough TUI in a headless terminal for visual iteration.
# Run via scripts/capture.sh (which ensures the server is up first).

Output "/tmp/bough_tui.gif"

Set Shell "bash"
Set FontSize 16
Set Width 1700
Set Height 950
Set Padding 12

# Launch the TUI against the running server, in a sandboxable workspace.
Type "cd /Users/andrey/repos/bough/packages/bough_tui && BOUGH_PROJECT=/tmp/boughwork BOUGH_SERVER=http://127.0.0.1:4096 gleam run"
Enter
Sleep 8s
Screenshot "/tmp/bough_1_empty.png"

Tab
Type "Use bash to list the files here, then tell me in one sentence what this project is."
Sleep 1s
Screenshot "/tmp/bough_2_typed.png"

Enter
Sleep 4s
Screenshot "/tmp/bough_3_thinking.png"

Sleep 22s
Screenshot "/tmp/bough_4_result.png"
