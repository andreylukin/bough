// Package secrets resolves project secret refs (keychain:<service>) on the
// host and stores asked secrets in the login keychain. Values never go
// into argv, errors or files; see docs/secrets.md.
package secrets

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

const scheme = "keychain:"

var ErrNotFound = errors.New("secret not found")

// Seams: tests swap these and never touch the user's keychain.
var (
	KeychainRead   = keychainRead
	KeychainWrite  = keychainWrite
	KeychainDelete = keychainDelete
)

// BOUGH_TEST_KEYCHAIN_DIR swaps both seams for a plaintext file per
// service in that dir, so a live run of the binary never touches the
// login keychain. Test use only.
func init() {
	if dir := os.Getenv("BOUGH_TEST_KEYCHAIN_DIR"); dir != "" {
		useFileKeychain(dir)
	}
}

func useFileKeychain(dir string) {
	file := func(service string) string { return filepath.Join(dir, strings.ReplaceAll(service, "/", "%")) }
	KeychainRead = func(service string) (string, error) {
		b, err := os.ReadFile(file(service))
		if errors.Is(err, os.ErrNotExist) {
			return "", fmt.Errorf("%w: keychain service %q", ErrNotFound, service)
		}
		return string(b), err
	}
	KeychainWrite = func(service, value string) error {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
		return os.WriteFile(file(service), []byte(value), 0o600)
	}
	KeychainDelete = func(service string) error {
		if err := os.Remove(file(service)); err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
		return nil
	}
}

var cache = struct {
	sync.Mutex
	m map[string]cached
}{m: map[string]cached{}}

type cached struct {
	val string
	err error
	at  time.Time
}

// failTTL caches a failed read briefly, so a locked keychain or an
// ignored access prompt does not stall every exec on the read timeout.
const failTTL = 30 * time.Second

// Resolve returns the value behind ref, cached per ref for five minutes.
// ErrNotFound when the item is missing; a failure is cached for failTTL,
// and Store drops it.
func Resolve(ref string) (string, error) {
	service, ok := strings.CutPrefix(ref, scheme)
	if !ok || service == "" {
		return "", fmt.Errorf("secrets: bad ref %q (want keychain:<service>)", ref)
	}
	cache.Lock()
	c, hit := cache.m[ref]
	cache.Unlock()
	if hit && c.err != nil && time.Since(c.at) < failTTL {
		return "", c.err
	}
	if hit && c.err == nil && time.Since(c.at) < 5*time.Minute {
		return c.val, nil
	}
	val, err := KeychainRead(service)
	cache.Lock()
	cache.m[ref] = cached{val, err, time.Now()}
	cache.Unlock()
	if err != nil {
		return "", err
	}
	return val, nil
}

// Refresh is Resolve reading the keychain now, not the cache, and
// keeping what it read. A check a person runs to see whether an item is
// there (preflight) must see one they just stored or deleted by hand.
func Refresh(ref string) (string, error) {
	cache.Lock()
	delete(cache.m, ref)
	cache.Unlock()
	return Resolve(ref)
}

// Store writes value to the keychain under service (create or update).
// Quotes, backslashes and newlines are refused: `security -i` tokenizes
// them.
func Store(service, value string) error {
	if strings.ContainsAny(value, "\"'\\\n\x00") {
		return errors.New("secrets: value has unsupported characters")
	}
	if err := KeychainWrite(service, value); err != nil {
		return err
	}
	cache.Lock()
	delete(cache.m, Ref(service))
	cache.Unlock()
	return nil
}

// Delete removes service from the keychain; a missing item is not an
// error.
func Delete(service string) error {
	if err := KeychainDelete(service); err != nil {
		return err
	}
	cache.Lock()
	delete(cache.m, Ref(service))
	cache.Unlock()
	return nil
}

// Service is the conventional service for an asked secret.
func Service(slug, name string) string { return "bough/" + slug + "/" + name }

func Ref(service string) string { return scheme + service }

func keychainRead(service string) (string, error) {
	// 30s leaves time for a keychain access prompt.
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	var stdout, stderr bytes.Buffer
	// No -a: a ref may name an item another tool created, any account.
	cmd := exec.CommandContext(ctx, "/usr/bin/security", "find-generic-password", "-s", service, "-w")
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) && ee.ExitCode() == 44 {
			return "", fmt.Errorf("%w: keychain service %q (security find-generic-password -s %s)", ErrNotFound, service, service)
		}
		return "", fmt.Errorf("secrets: read %s: %w: %s", service, err, strings.TrimSpace(stderr.String()))
	}
	return strings.TrimSuffix(stdout.String(), "\n"), nil
}

func keychainDelete(service string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	var out bytes.Buffer
	cmd := exec.CommandContext(ctx, "/usr/bin/security", "delete-generic-password", "-s", service)
	cmd.Stdout, cmd.Stderr = &out, &out
	if err := cmd.Run(); err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) && ee.ExitCode() == 44 {
			return nil
		}
		return fmt.Errorf("secrets: delete %s: %w: %s", service, err, strings.TrimSpace(out.String()))
	}
	return nil
}

// keychainWrite passes the command on stdin to `security -i`, so the value
// never shows up in argv or ps. `security -i` exits 0 when a subcommand
// fails, so the write is confirmed by reading it back.
func keychainWrite(service, value string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	var out bytes.Buffer
	cmd := exec.CommandContext(ctx, "/usr/bin/security", "-i")
	cmd.Stdin = strings.NewReader(fmt.Sprintf("add-generic-password -U -a bough -s \"%s\" -w \"%s\"\n", service, value))
	cmd.Stdout, cmd.Stderr = &out, &out
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("secrets: store %s: %w: %s", service, err, strings.ReplaceAll(out.String(), value, "[secret]"))
	}
	if got, err := keychainRead(service); err != nil || got != value {
		msg := strings.TrimSpace(strings.ReplaceAll(out.String(), value, "[secret]"))
		if err != nil {
			return fmt.Errorf("secrets: store %s: not written: %s", service, strings.ReplaceAll(err.Error(), value, "[secret]")+" "+msg)
		}
		return fmt.Errorf("secrets: store %s: read back a different value: %s", service, msg)
	}
	return nil
}
