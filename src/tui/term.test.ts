import { assertEquals } from "jsr:@std/assert@1";
import {
  isFocused,
  parseBgSpec,
  reportTermBg,
  sanitize,
  setFocused,
  tmuxWrap,
  termBackground,
} from "./term.ts";

Deno.test("sanitize strips control bytes from titles/bodies", () => {
  assertEquals(sanitize("ok\x07\x1b]0;evil\x07"), "ok  ]0;evil ");
  assertEquals(sanitize("plain title"), "plain title");
});

Deno.test("tmuxWrap doubles every ESC and wraps in the passthrough DCS", () => {
  assertEquals(tmuxWrap("\x1b]9;hi\x07", true), "\x1bPtmux;\x1b\x1b]9;hi\x07\x1b\\");
  assertEquals(tmuxWrap("\x1b]9;hi\x07", false), "\x1b]9;hi\x07");
});

Deno.test("parseBgSpec scales 16/8/4-bit channels to #rrggbb", () => {
  assertEquals(parseBgSpec("rgb:1e1e/1e1e/2e2e"), "#1e1e2e");
  assertEquals(parseBgSpec("rgb:fa/fa/fa"), "#fafafa");
  assertEquals(parseBgSpec("rgb:f/0/f"), "#ff00ff");
  assertEquals(parseBgSpec("not-a-color"), null);
});

Deno.test("termBackground classifies dark vs light", () => {
  reportTermBg("rgb:1e1e/1e1e/2e2e");
  assertEquals(termBackground(), { hex: "#1e1e2e", scheme: "dark" });
  reportTermBg("rgb:fafa/fafa/fafa");
  assertEquals(termBackground(), { hex: "#fafafa", scheme: "light" });
  reportTermBg("garbage"); // a bad report never clobbers a good one
  assertEquals(termBackground()?.hex, "#fafafa");
});

Deno.test("focus state tracks the last report", () => {
  setFocused(false);
  assertEquals(isFocused(), false);
  setFocused(true);
  assertEquals(isFocused(), true);
});
