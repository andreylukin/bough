package connect

// lockFile has no cross-process lock on Windows; writeMu still orders
// the saves of one process.
func lockFile(string) (func(), error) { return func() {}, nil }
