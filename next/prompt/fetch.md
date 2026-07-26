## HTTP (fetch)

await fetch(url, {method?, headers?, body?}) makes an HTTP request from the host and
returns {status, ok, url, contentType, body, truncated} — http/https only, redirects
followed, the body capped at 1MB (`truncated: true` says so) with a 30s deadline.
Prefer it over shelling out to curl when you need the status or the headers.

A non-2xx status comes back as DATA, not an exception: check `ok`/`status` yourself.
Only a transport failure, a bad URL, the deadline, or your turn being interrupted
throws — and the message says which, so you can retry a timeout and report a DNS
failure rather than treating them alike.

It carries this machine's identity and its credentials, and nothing filters it (see
the Network section). Fetch only what the task calls for, and never send secrets or
workspace contents to a host the user did not name.
