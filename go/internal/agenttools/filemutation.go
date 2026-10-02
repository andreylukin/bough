package agenttools

import (
	"context"
	"path/filepath"
)

// FileMutation coordinates a guarded native file mutation with any
// editable projection that owns the path. Unrelated paths need no lock.
type FileMutation func(context.Context, string, func() error) error

type fileMutationKey struct{}

func WithFileMutation(ctx context.Context, fn FileMutation) context.Context {
	return context.WithValue(ctx, fileMutationKey{}, fn)
}

// MutateFile belongs at the file row's guarded write boundary: hooks,
// permission waits, formatting and diagnostics must not hold its lock.
func MutateFile(ctx context.Context, path string, mutate func() error) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if fn, ok := ctx.Value(fileMutationKey{}).(FileMutation); ok {
		return fn(ctx, path, mutate)
	}
	return mutate()
}

// CanonicalFile identifies the same target through relative paths and
// symlinked parent directories, including a target not yet created.
func CanonicalFile(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	rest := ""
	for p := abs; ; p = filepath.Dir(p) {
		if resolved, err := filepath.EvalSymlinks(p); err == nil {
			return filepath.Join(resolved, rest), nil
		}
		if filepath.Dir(p) == p {
			return abs, nil
		}
		rest = filepath.Join(filepath.Base(p), rest)
	}
}
