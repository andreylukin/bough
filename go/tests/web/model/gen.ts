// Usage: node model/gen.ts <fizz run dir> [out.json]
// Writes the covering paths as JSON (to stdout without out.json) and a
// one-line coverage report to stderr, or to stdout when writing a file.
import { writeFileSync } from 'node:fs';
import { loadGraph, generate } from './graph.ts';

const [dir, outFile] = process.argv.slice(2);
if (!dir) {
  process.stderr.write('usage: node model/gen.ts <fizz run dir> [out.json]\n');
  process.exit(2);
}
try {
  const out = generate(loadGraph(dir));
  const json = JSON.stringify(out, null, 2) + '\n';
  const { states, transitions } = out.coverage;
  const report = `states ${states.covered}/${states.total}, transitions ${transitions.covered}/${transitions.total}, paths ${out.paths.length}\n`;
  if (outFile) {
    writeFileSync(outFile, json);
    process.stdout.write(report);
  } else {
    process.stdout.write(json);
    process.stderr.write(report);
  }
} catch (e) {
  process.stderr.write(`gen: ${(e as Error).message}\n`);
  process.exit(1);
}
