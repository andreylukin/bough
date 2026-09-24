// Command boughbin prints testbin.Path: the binary the Go suites
// exec, for suites outside Go (tests/web's global setup) to reuse
// instead of linking their own.
package main

import (
	"fmt"
	"os"

	"github.com/andreylukin/bough/internal/testbin"
)

func main() {
	bin, err := testbin.Path()
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println(bin)
}
