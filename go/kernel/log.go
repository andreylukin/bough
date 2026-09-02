package kernel

import (
	"fmt"
	"os"
)

// Verbose gates diagnostic chatter — row reloads, MCP tool bindings,
// which config file loaded. Off by default so the CLI's stderr is quiet;
// --verbose or BOUGH_VERBOSE=1 turns it on. Failures and warnings are
// always printed.
var Verbose = os.Getenv("BOUGH_VERBOSE") == "1"

// Logf prints a diagnostic line to stderr when Verbose is set.
func Logf(format string, args ...any) {
	if Verbose {
		fmt.Fprintf(os.Stderr, format, args...)
	}
}
