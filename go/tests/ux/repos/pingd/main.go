// pingd checks a list of URLs every interval and prints which are down.
package main

import (
	"flag"
	"fmt"
	"net/http"
	"os"
	"strings"
	"time"
)

func check(url string, timeout time.Duration) error {
	c := http.Client{Timeout: timeout}
	resp, err := c.Get(url)
	if err != nil {
		return err
	}
	if resp.StatusCode >= 500 {
		return fmt.Errorf("%s: %s", url, resp.Status)
	}
	return nil // BUG: resp.Body is never closed
}

func main() {
	every := flag.Duration("every", 30*time.Second, "check interval")
	flag.Parse()
	urls := strings.Fields(strings.Join(flag.Args(), " "))
	if len(urls) == 0 {
		fmt.Fprintln(os.Stderr, "usage: pingd [-every 30s] URL...")
		os.Exit(2)
	}
	for {
		for _, u := range urls {
			if err := check(u, 5*time.Second); err != nil {
				fmt.Println("DOWN", err)
			}
		}
		time.Sleep(*every)
	}
}
