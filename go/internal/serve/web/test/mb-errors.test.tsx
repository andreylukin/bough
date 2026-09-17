import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { providerError } from "../src/loading";
import { WikiPageView } from "../src/wiki";

test("MB-ERR: providerError names the provider and status, and lifts the JSON message", () => {
  const a = providerError('llm-anthropic: POST "https://api.anthropic.com/v1/messages": 401 Unauthorized {"type":"error","error":{"type":"authentication_error","message":"API key is invalid."}}');
  expect(a.title).toBe("Anthropic returned 401 Unauthorized");
  expect(a.body).toBe("API key is invalid. Check the key in Settings, or switch to another model.");
  expect(providerError("llm-openrouter: HTTP 401: Missing Authentication header").title).toBe("OpenRouter returned 401");
  expect(providerError("wiki: something broke")).toEqual({ title: "Something broke", body: "" });
  expect(providerError("Failed to fetch").title).toBe("The server did not answer. Check that bough serve is running.");
});

test("MB-ERR: the toast speaks humanError and leaves Escape to the turn", () => {
  const src = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  const toast = src.slice(src.indexOf('<div className="toast"'), src.indexOf("</div>\n      ) : null}", src.indexOf('<div className="toast"')));
  expect(toast).toContain("{humanError(toast.msg)}");
  expect(toast).not.toContain("— {err.msg}");
  expect(toast).toContain('aria-label="Dismiss"');
  expect(toast).not.toMatch(/Escape|onKeyDown/);
});

test("MB-ERR: a missing wiki page is a neutral fact: no red glyph, no details, Search the wiki", () => {
  const noop = () => {};
  const html = renderToStaticMarkup(
    <WikiPageView page={null} path="topics/bough/does-not-exist.md" pageError="wiki: not found" onCite={noop} onCloseSource={noop}
                  onOpenPage={noop} onIndex={noop} onSearch={noop} />,
  );
  expect(html).toContain("This page doesn’t exist");
  expect(html).toContain("topics/bough/does-not-exist.md");
  expect(html).not.toContain("error-dot");
  expect(html).not.toContain("state-icon-alert");
  expect(html).not.toContain("<details");
  expect(html).toContain("Search the wiki");
  expect(html).not.toContain(">Page not found<");
});
