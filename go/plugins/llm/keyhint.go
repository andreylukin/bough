package llm

import (
	"fmt"
	"net/http"
)

// keyRejected is the error for a 401: the provider refused the key. A
// first run with a mistyped key used to show the raw body ("401
// Unauthorized {"type":"authentication_error",...}"), which says what
// broke but not where the key lives or how to replace it.
func keyRejected(plugin, env, provider string, status int, detail string) error {
	if detail == "" {
		detail = http.StatusText(status)
	}
	return fmt.Errorf("%s: %s was rejected (HTTP %d: %s). Replace it with /connect %s <key>, or edit ~/.bough/env", plugin, env, status, detail, provider)
}
