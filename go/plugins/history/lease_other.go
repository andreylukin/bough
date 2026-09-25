//go:build !unix

package history

// Without flock there is no lease: every take succeeds and nothing is
// ever held, as before leases existed.
func TakeLease(string) (func(), error) { return func() {}, nil }

func LeaseHolder(string) int { return 0 }
