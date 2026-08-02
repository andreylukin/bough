/**
 * Timestamp stripping and value masking — the two stages that decide what a
 * "structurally identical line" even means. Most of these tests are order
 * regressions: they pin the alternation order in `mask.ts`, which is the part that
 * silently degrades rather than failing loudly when it is wrong.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mask } from "./mask.ts";
import { stripTimestamp } from "./timestamp.ts";

/** The kinds a line masks to, in order — the shape most assertions here care about. */
function kinds(line: string): string[] {
  return mask(line).values.map((v) => v.kind);
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

test("stripTimestamp parses ISO 8601 with fraction and zone", () => {
  const r = stripTimestamp("2024-01-15T14:22:01.100Z INFO up");
  assert.equal(r.rest, "INFO up");
  assert.equal(r.when, Date.UTC(2024, 0, 15, 14, 22, 1, 100));
});

test("stripTimestamp applies the UTC offset rather than ignoring it", () => {
  // Getting this backwards is the classic bug and it is invisible in the output —
  // the span is simply wrong by hours.
  const utc = stripTimestamp("2024-01-15T14:22:01Z x").when as number;
  const plus = stripTimestamp("2024-01-15T19:52:01+05:30 x").when as number;
  assert.equal(plus, utc);
  const minus = stripTimestamp("2024-01-15T09:22:01-05:00 x").when as number;
  assert.equal(minus, utc);
});

test("stripTimestamp pads fractional seconds instead of parsing them as a decimal", () => {
  // `.1` is 100ms. Read as a decimal it is 1ms, and every sub-second span collapses.
  assert.equal(stripTimestamp("2024-01-15T00:00:00.1Z x").when, Date.UTC(2024, 0, 15, 0, 0, 0, 100));
  assert.equal(
    stripTimestamp("2024-01-15T00:00:00.123456Z x").when,
    Date.UTC(2024, 0, 15, 0, 0, 0, 123),
  );
});

test("stripTimestamp handles the bracketed, apache and syslog forms", () => {
  const b = stripTimestamp("[2024-01-15 14:22:01] boot");
  assert.equal(b.rest, "boot");
  assert.equal(b.when, Date.UTC(2024, 0, 15, 14, 22, 1));

  const a = stripTimestamp("15/Jan/2024:14:22:01 +0000 GET /");
  assert.equal(a.when, Date.UTC(2024, 0, 15, 14, 22, 1));

  const s = stripTimestamp("Jan 15 14:22:01 host sshd: in", 2024);
  assert.equal(s.rest, "host sshd: in");
  assert.equal(s.when, Date.UTC(2024, 0, 15, 14, 22, 1));
});

test("stripTimestamp tells epoch seconds from milliseconds by width", () => {
  assert.equal(stripTimestamp("1705328521 up").when, 1705328521000);
  assert.equal(stripTimestamp("1705328521123 up").when, 1705328521123);
});

test("stripTimestamp does not read a long id as an epoch", () => {
  // The trailing-boundary guard. Without it a 16-digit request id becomes a date
  // and the analysis reports a time span spanning centuries.
  const r = stripTimestamp("1705328521123456 request done");
  assert.equal(r.when, undefined);
  assert.equal(r.rest, "1705328521123456 request done");
});

test("stripTimestamp leaves an unstamped line entirely alone", () => {
  // Build output and stack traces have no timestamp and must still cluster.
  const r = stripTimestamp("  at Object.<anonymous> (/app/index.js:1:1)");
  assert.equal(r.when, undefined);
  assert.equal(r.rest, "  at Object.<anonymous> (/app/index.js:1:1)");
});

// ---------------------------------------------------------------------------
// Masking: the template
// ---------------------------------------------------------------------------

test("mask collapses two executions of one statement to one logtype", () => {
  // The entire point of the stage.
  const a = mask("Request from 10.0.1.15 completed in 45ms status=200");
  const b = mask("Request from 10.0.2.99 completed in 1.2s status=404");
  assert.equal(a.logtype, b.logtype);
  assert.equal(a.logtype, "Request from <ipv4> completed in <duration> status=<int>");
});

test("mask keeps structurally different lines apart", () => {
  // Typed placeholders rather than a bare `<*>`: connecting to an address and
  // connecting to a hostname are different statements.
  assert.notEqual(mask("connect to 10.0.1.15").logtype, mask("connect to db-primary").logtype);
});

// ---------------------------------------------------------------------------
// Masking: alternation order
// ---------------------------------------------------------------------------

test("an IPv4 is one value, not a float and two ints", () => {
  assert.deepEqual(kinds("from 10.0.1.15 ok"), ["ipv4"]);
});

test("an address with a port is an address and a port", () => {
  const m = mask("connect 10.0.1.15:5432 failed");
  assert.equal(m.logtype, "connect <ipv4>:<int> failed");
});

test("a UUID survives whole", () => {
  assert.deepEqual(kinds("trace 550e8400-e29b-41d4-a716-446655440000 done"), ["uuid"]);
});

test("a URL is one value, not a scheme plus a path plus numbers", () => {
  const m = mask("GET https://api.example.com:8443/v1/users?id=42 -> 200");
  assert.equal(m.logtype, "GET <url> -> <int>");
});

test("a quoted string is opaque", () => {
  // Values inside a message belong to the message. Matching into them splits one
  // variable into five and makes every distinct message its own pattern.
  const m = mask(`msg="connect to 10.0.1.15 failed after 3s" code=500`);
  assert.equal(m.logtype, "msg=<quoted> code=<int>");
});

test("a size is a size and a duration is a duration", () => {
  assert.deepEqual(kinds("wrote 5MB in 250ms"), ["bytes", "duration"]);
});

test("a bare hex id needs eight characters, and that is the only rule", () => {
  assert.deepEqual(kinds("session abc123def closed"), ["hex"]);
  // An all-letter hex id must match too. Requiring a digit as well — to keep words
  // out — leaves `bebbccce` unmasked, and an unmasked id indexes literally in the
  // clustering tree and becomes its own singleton pattern. That produced a hundred
  // junk patterns on a 500k-line log.
  assert.deepEqual(kinds("session bebbccce closed"), ["hex"]);
  // Short words stay words; `accede` is six characters.
  assert.deepEqual(kinds("the request will accede shortly"), []);
});

test("a path is one value", () => {
  const m = mask("reading /var/log/app2.log now");
  assert.equal(m.logtype, "reading <path> now");
});

test("a lone slash between words is not a path", () => {
  assert.deepEqual(kinds("mode read/write enabled"), []);
});

test("a clock time is not an IPv6 address", () => {
  // The conservative IPv6 pattern earns its keep here: a permissive one turns every
  // mid-line `14:22:01` into an address.
  assert.deepEqual(kinds("elapsed 14:22:01 total"), ["int", "int", "int"]);
  assert.deepEqual(kinds("peer fe80::1 up"), ["ipv6"]);
  assert.deepEqual(kinds("peer 2001:0db8:85a3:0000:0000:8a2e:0370:7334 up"), ["ipv6"]);
});

test("an out-of-range dotted quad is not an address", () => {
  // Version strings are dotted quads too, and octet checking is what tells them
  // apart. `4000` cannot be an octet.
  assert.notEqual(kinds("version 1.2.3.4000 built")[0], "ipv4");
});

test("a unit needs no space, and a word after a number is not a unit", () => {
  assert.deepEqual(kinds("took 5s"), ["duration"]);
  // `5 minutes` must not read as five minutes-the-unit plus stray text; the space
  // rule is what prevents it.
  assert.deepEqual(kinds("waited 5 minutes"), ["int"]);
  assert.deepEqual(kinds("scale 5m"), ["duration"]);
});

// ---------------------------------------------------------------------------
// Masking: magnitudes
// ---------------------------------------------------------------------------

test("durations normalize to milliseconds so a slot can be ranked", () => {
  // Without normalization a p99 over `1.5s` and `900ms` ranks 900 above 1.5 and
  // reports the fast case as the slow one — an inverted answer, not a rounding one.
  const n = (s: string) => mask(`took ${s}`).values[0]?.num;
  assert.equal(n("500ms"), 500);
  assert.equal(n("1.5s"), 1500);
  assert.equal(n("2m"), 120000);
  assert.equal(n("100us"), 0.1);
});

test("sizes normalize to bytes", () => {
  const n = (s: string) => mask(`wrote ${s}`).values[0]?.num;
  assert.equal(n("1KB"), 1024);
  assert.equal(n("2MB"), 2 * 1024 * 1024);
});

test("kinds without a magnitude carry none", () => {
  assert.equal(mask("from 10.0.1.15").values[0]?.num, undefined);
});

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

test("mask is stateless across calls", () => {
  // The combined regex is a module singleton carrying `g`. A stale `lastIndex`
  // silently skips the head of the next line, which shows up as a mysterious
  // second cluster for a statement that should have had one.
  const line = "from 10.0.1.15 in 5ms";
  const first = mask(line);
  for (let i = 0; i < 5; i++) assert.deepEqual(mask(line), first);
});

test("mask handles an empty line and a line with no values", () => {
  assert.deepEqual(mask(""), { logtype: "", values: [] });
  assert.deepEqual(mask("server started"), { logtype: "server started", values: [] });
});
