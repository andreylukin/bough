import { expect, test } from "bun:test";
const { keyLine, pickProvider } = await import("../src/welcome");

const p = (name: string, set: boolean) => ({ name, env: name.toUpperCase() + "_API_KEY", set });

// R3-I: "Key found for Anthropic" while every session on that key 401'd.
test("a set key reads as working, rejected or checking, never just found", () => {
  const providers = [p("anthropic", true), p("openai", true), p("cerebras", false)];
  expect(keyLine(providers, { anthropic: "checking", openai: "checking" }).text).toContain("Checking");
  const mixed = keyLine(providers, { anthropic: "rejected", openai: "ok" });
  expect(mixed.text).toContain("OpenAI key works");
  expect(mixed.text).toContain("Anthropic key was rejected");
  expect(mixed.tone).toBe("warn");
  const bad = keyLine([p("anthropic", true)], { anthropic: "rejected" });
  expect(bad.tone).toBe("err");
  expect(bad.working).toBe(false);
  expect(keyLine([p("anthropic", true)], { anthropic: "unknown" }).working).toBe(true);
});

test("the key form defaults to a provider without a working key", () => {
  expect(pickProvider([p("anthropic", true), p("openai", false)], { anthropic: "ok" })).toBe("openai");
  expect(pickProvider([p("anthropic", true), p("openai", true)], { anthropic: "rejected", openai: "ok" })).toBe("anthropic");
});

// R4-C: a rejected key never wears the done badge, and plural keys "work".
test("step 1 is not done while any set key is rejected", () => {
  const { stepDone } = require("../src/welcome");
  const mixed = keyLine([p("anthropic", true), p("openai", true)], { anthropic: "rejected", openai: "ok" });
  expect(stepDone(mixed)).toBe(false);
  expect(stepDone(keyLine([p("anthropic", true)], { anthropic: "ok" }))).toBe(true);
  expect(keyLine([p("anthropic", true), p("openai", true)], { anthropic: "ok", openai: "ok" }).text).toContain("keys work");
});
