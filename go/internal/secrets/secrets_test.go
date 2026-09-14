package secrets

import (
	"errors"
	"strings"
	"testing"
)

// Not parallel: the tests swap the package-level seams.
func fakeKeychain(t *testing.T) (map[string]string, *int) {
	t.Helper()
	store := map[string]string{}
	reads := 0
	oldR, oldW := KeychainRead, KeychainWrite
	KeychainRead = func(s string) (string, error) {
		reads++
		v, ok := store[s]
		if !ok {
			return "", ErrNotFound
		}
		return v, nil
	}
	KeychainWrite = func(s, v string) error { store[s] = v; return nil }
	t.Cleanup(func() {
		KeychainRead, KeychainWrite = oldR, oldW
		cache.Lock()
		cache.m = map[string]cached{}
		cache.Unlock()
	})
	return store, &reads
}

func TestResolveStoreCache(t *testing.T) {
	store, reads := fakeKeychain(t)
	svc := Service("web", "DEVPI_URL")
	if svc != "bough/web/DEVPI_URL" || Ref(svc) != "keychain:bough/web/DEVPI_URL" {
		t.Fatalf("service %q ref %q", svc, Ref(svc))
	}
	if _, err := Resolve(Ref(svc)); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing: %v", err)
	}
	if _, err := Resolve(Ref(svc)); !errors.Is(err, ErrNotFound) || *reads != 1 {
		t.Fatalf("miss not cached: reads %d", *reads)
	}
	if err := Store(svc, "https://a"); err != nil {
		t.Fatal(err)
	}
	for range 2 {
		if v, err := Resolve(Ref(svc)); err != nil || v != "https://a" {
			t.Fatalf("resolve %q %v", v, err)
		}
	}
	if *reads != 2 {
		t.Fatalf("hit not cached: reads %d", *reads)
	}
	store[svc] = "stale-bypass"
	if err := Store(svc, "https://b"); err != nil {
		t.Fatal(err)
	}
	if v, _ := Resolve(Ref(svc)); v != "https://b" {
		t.Fatalf("Store did not drop the cache: %q", v)
	}
	if _, err := Resolve("vault:x"); err == nil {
		t.Fatal("bad scheme resolved")
	}
}

func TestStoreRejectsCharacters(t *testing.T) {
	store, _ := fakeKeychain(t)
	for _, v := range []string{`a"b`, `a'b`, `a\b`, "a\nb", "a\x00b"} {
		if err := Store("bough/x/Y", v); err == nil || err.Error() != "secrets: value has unsupported characters" {
			t.Errorf("%q: %v", v, err)
		}
	}
	if len(store) != 0 {
		t.Fatalf("stored %v", store)
	}
}

func TestFileKeychain(t *testing.T) {
	fakeKeychain(t) // restores the seams afterwards
	useFileKeychain(t.TempDir())
	if _, err := KeychainRead("bough/p/X"); !errors.Is(err, ErrNotFound) || !strings.Contains(err.Error(), `"bough/p/X"`) {
		t.Fatalf("missing: %v", err)
	}
	if err := Store("bough/p/X", "v1"); err != nil {
		t.Fatal(err)
	}
	if v, err := Resolve("keychain:bough/p/X"); err != nil || v != "v1" {
		t.Fatalf("resolve %q %v", v, err)
	}
}
