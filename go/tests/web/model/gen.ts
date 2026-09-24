// Usage: node model/gen.ts <fizz run dir>
// Prints the walks the tests derive from a graph (default: every settled
// state; MODEL_COVER=transitions: every link) as JSON on stdout, and a
// one-line coverage report for each cover on stderr. Nothing is written
// to the tree: the tests derive the same walks at run time.
import { loadGraph, walks, type Cover } from './graph.ts';

const [dir] = process.argv.slice(2);
if (!dir) {
  process.stderr.write('usage: node model/gen.ts <fizz run dir>\n');
  process.exit(2);
}
try {
  const g = loadGraph(dir);
  for (const cover of ['states', 'transitions'] as Cover[]) {
    const out = walks(g, cover);
    const steps = out.paths.reduce((n, p) => n + p.trace.length - 1, 0);
    const { states, transitions } = out.coverage;
    process.stderr.write(`${cover}: walks ${out.paths.length}, steps ${steps}, states ${states.covered}/${states.total}, transitions ${transitions.covered}/${transitions.total}\n`);
  }
  const cover: Cover = process.env.MODEL_COVER === 'transitions' ? 'transitions' : 'states';
  process.stdout.write(JSON.stringify(walks(g, cover), null, 2) + '\n');
} catch (e) {
  process.stderr.write(`gen: ${(e as Error).message}\n`);
  process.exit(1);
}
