// The design cards link ../bough.css, a copy of the <style> block in
// dist/index.html. A copy drifts in silence, so this compares the two
// and fails when they differ. Run `bun run design:check`; fix with
// `bun run design:sync`.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const shell = readFileSync(join(here, "..", "dist", "index.html"), "utf8");
const m = /^<style>\n([\s\S]*?)^<\/style>\n/m.exec(shell);
if (!m) { console.error("design/check: no <style> block in dist/index.html"); process.exit(2); }
const cssPath = join(here, "bough.css");
if (process.argv.includes("--write")) {
  writeFileSync(cssPath, m[1]);
  console.log("design/check: wrote bough.css from dist/index.html");
} else if (readFileSync(cssPath, "utf8") !== m[1]) {
  console.error("design/check: bough.css differs from the <style> in dist/index.html — run `bun run design:sync`");
  process.exit(1);
} else {
  console.log("design/check: bough.css matches dist/index.html");
}
