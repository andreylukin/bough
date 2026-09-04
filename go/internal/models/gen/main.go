// Command gen regenerates the embedded model-catalogue snapshot from
// models.dev's api.json:
//
//	curl -s https://models.dev/api.json > /tmp/api.json
//	go run ./internal/models/gen -in /tmp/api.json -out internal/models/snapshot.json.gz
//
// The snapshot is the offline answer: a fresh install with no network
// still prices what it runs. It is trimmed to the providers bough has
// plugins for, which is ~6 KB compressed against the 4 MB original.
package main

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"flag"
	"fmt"
	"os"

	"github.com/andreylukin/bough/internal/models"
)

func main() {
	in := flag.String("in", "", "path to models.dev api.json")
	out := flag.String("out", "internal/models/snapshot.json.gz", "snapshot to write")
	flag.Parse()
	if *in == "" {
		fmt.Fprintln(os.Stderr, "usage: gen -in api.json [-out snapshot.json.gz]")
		os.Exit(2)
	}
	raw, err := os.ReadFile(*in)
	if err != nil {
		fatal(err)
	}
	cat, err := models.Trim(raw)
	if err != nil {
		fatal(err)
	}
	body, err := json.Marshal(cat)
	if err != nil {
		fatal(err)
	}
	var buf bytes.Buffer
	zw, _ := gzip.NewWriterLevel(&buf, gzip.BestCompression)
	if _, err := zw.Write(body); err != nil {
		fatal(err)
	}
	if err := zw.Close(); err != nil {
		fatal(err)
	}
	if err := os.WriteFile(*out, buf.Bytes(), 0o644); err != nil {
		fatal(err)
	}
	n := 0
	for _, ms := range cat {
		n += len(ms)
	}
	fmt.Printf("%s: %d providers, %d models, %d bytes\n", *out, len(cat), n, buf.Len())
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "gen:", err)
	os.Exit(1)
}
