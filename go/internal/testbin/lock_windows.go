package testbin

// lock is a no-op on Windows: concurrent builders then each link, as
// before, and the rename into place still keeps the cache consistent.
func lock(string) (func(), error) { return func() {}, nil }
