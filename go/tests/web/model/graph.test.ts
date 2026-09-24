// Unit tests for the path generator, on real fizz v0.5.3 run dirs checked
// in under go/tests/model/testdata (light/ is example/Light.fizz, door/ is
// testdata/Door.fizz). Run: npm run test:model (node strips the types).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadGraph, generate, type Graph, type Output } from './graph.ts';

const here = dirname(fileURLToPath(import.meta.url));
const testdata = join(here, '..', '..', 'model', 'testdata');

// The links a path walks, as "src-Name->dest", for readable assertions.
const walk = (g: Graph, links: number[]) =>
  links.map((i) => `${g.links[i].src}-${g.links[i].name}->${g.links[i].dest}`);

test('decodes the light run dir', () => {
  const g = loadGraph(join(testdata, 'light'));
  assert.deepEqual(g.nodes.map((n) => n.state), [{ color: 'red' }, { color: 'green' }, { color: 'yellow' }]);
  assert.deepEqual(walk(g, g.links.map((l) => l.index)), ['0-Go->1', '1-Slow->2', '2-Stop->0']);
  assert.deepEqual(g.links.map((l) => l.type), ['action', 'action', 'action']);
});

// example/ is go/tests/model/specs/example.fizz: its state is a role's,
// which the node JSON keeps under roles[], not state. The generator reads
// it the way go/tests/model/tracecheck does, "<Role>#<i>.<field>".
test('decodes role fields as qualified state', () => {
  const g = loadGraph(join(testdata, 'example'));
  assert.deepEqual(g.nodes[0].state, {
    session: 'role Session#0',
    'Session#0.status': 'idle', 'Session#0.unseen': false, 'Session#0.viewing': false,
  });
  const out = generate(g);
  assert.equal(out.coverage.transitions.uncovered.length, 0);
  assert.ok(out.paths.every((p) => p.trace.slice(1).every((s) => s.action.startsWith('Session#0.'))));
});

test('light: one path covers the cycle', () => {
  const g = loadGraph(join(testdata, 'light'));
  const out = generate(g);
  assert.equal(out.paths.length, 1);
  assert.deepEqual(walk(g, out.paths[0].links), ['0-Go->1', '1-Slow->2', '2-Stop->0']);
  assert.deepEqual(out.paths[0].trace, [
    { action: 'Init', state: { color: 'red' } },
    { action: 'Go', state: { color: 'green' } },
    { action: 'Slow', state: { color: 'yellow' } },
    { action: 'Stop', state: { color: 'red' } },
  ]);
  assert.deepEqual(out.coverage.states, { covered: 3, total: 3, uncovered: [] });
  assert.deepEqual(out.coverage.transitions, { covered: 3, total: 3, uncovered: [] });
});

test('door: every link covered, each through a shortest prefix, ending settled', () => {
  const g = loadGraph(join(testdata, 'door'));
  const out = generate(g);
  assert.deepEqual(out.coverage.transitions, { covered: g.links.length, total: 7, uncovered: [] });
  assert.deepEqual(out.coverage.states, { covered: 4, total: 4, uncovered: [] });

  const covered = new Set(out.paths.flatMap((p) => p.links));
  assert.equal(covered.size, g.links.length);

  // BFS depth from the initial state, computed independently here.
  const depth = new Map<number, number>([[0, 0]]);
  for (let frontier = [0]; frontier.length; ) {
    const next: number[] = [];
    for (const n of frontier) for (const l of g.links) {
      if (l.src === n && !depth.has(l.dest)) { depth.set(l.dest, depth.get(n)! + 1); next.push(l.dest); }
    }
    frontier = next;
  }
  for (const p of out.paths) {
    const target = g.links[p.target];
    const at = p.links.indexOf(p.target);
    assert.equal(at, depth.get(target.src), `target ${target.name} is reached by a shortest prefix`);
    for (let i = 1; i < p.links.length; i++) {
      assert.equal(g.links[p.links[i]].src, g.links[p.links[i - 1]].dest, 'path is connected');
    }
    assert.equal(g.links[p.links[0]].src, 0, 'path starts at the initial state');
    assert.equal(g.nodes[g.links[p.links.at(-1)!].dest].name, 'yield', 'path ends in a settled state');
    // The fork links fold into the Slam step: the trace names actions only.
    for (const s of p.trace) assert.ok(!s.action.startsWith('Any:'), `trace step ${s.action}`);
  }
  // Slam's two outcomes each need their own path.
  const slams = out.paths.filter((p) => p.trace.some((s) => s.action === 'Slam'));
  assert.deepEqual(slams.map((p) => p.trace.find((s) => s.action === 'Slam')!.state).sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b))),
    [{ door: 'closed', locked: false }, { door: 'closed', locked: true }]);
});

test('reports links unreachable from the initial state as uncovered', () => {
  const g: Graph = {
    nodes: [0, 1, 2].map((i) => ({ index: i, name: 'yield', state: { n: i } })),
    links: [
      { index: 0, src: 0, dest: 1, name: 'A', type: 'action' },
      { index: 1, src: 2, dest: 1, name: 'B', type: 'action' },
    ],
  };
  const out = generate(g);
  assert.deepEqual(out.coverage.transitions, { covered: 1, total: 2, uncovered: [1] });
  assert.deepEqual(out.coverage.states, { covered: 2, total: 3, uncovered: [2] });
});

test('a path into a fork is extended to a settled state', () => {
  // Two actions reach the same intermediate node F; the deeper one (1-B->F)
  // is the target of its own path, which must not stop inside B.
  const g: Graph = {
    nodes: [
      { index: 0, name: 'yield', state: { x: 0 } },
      { index: 1, name: 'yield', state: { x: 1 } },
      { index: 2, name: 'B', state: { x: 1 } },
      { index: 3, name: 'yield', state: { x: 2 } },
    ],
    links: [
      { index: 0, src: 0, dest: 1, name: 'A', type: 'action' },
      { index: 1, src: 0, dest: 2, name: 'B', type: 'action' },
      { index: 2, src: 1, dest: 2, name: 'B', type: 'action' },
      { index: 3, src: 2, dest: 3, name: 'Any:x=2', type: '' },
    ],
  };
  const out = generate(g);
  const p = out.paths.find((p) => p.target === 2)!;
  assert.deepEqual(walk(g, p.links), ['0-A->1', '1-B->2', '2-Any:x=2->3']);
  assert.deepEqual(p.trace.at(-1), { action: 'B', state: { x: 2 } });
  assert.deepEqual(out.coverage.transitions.uncovered, []);
});

// go/tests/model/tracecheck replays door's traces and the browser model
// specs walk every other spec's, so none may drift from its graph.
for (const name of readdirSync(testdata).filter((d) => existsSync(join(testdata, d, 'paths.json')))) {
  test(`the checked-in ${name} paths.json is what the generator writes`, () => {
    const want = readFileSync(join(testdata, name, 'paths.json'), 'utf8');
    const got = JSON.stringify(generate(loadGraph(join(testdata, name))), null, 2) + '\n';
    assert.equal(got, want, `stale; run: scripts/model-test.sh gen ${name}`);
  });
}

test('cli writes the JSON and prints the coverage report', () => {
  const dir = mkdtempSync(join(tmpdir(), 'gen-'));
  const outFile = join(dir, 'paths.json');
  const stdout = execFileSync(process.execPath, [join(here, 'gen.ts'), join(testdata, 'door'), outFile], { encoding: 'utf8' });
  assert.match(stdout, /states 4\/4, transitions 7\/7, paths \d+/);
  const out: Output = JSON.parse(readFileSync(outFile, 'utf8'));
  assert.equal(out.coverage.transitions.covered, 7);
});

test('cli fails on a dir with no graph', () => {
  const dir = mkdtempSync(join(tmpdir(), 'gen-'));
  mkdirSync(join(dir, 'empty'));
  writeFileSync(join(dir, 'empty', 'state_config.json'), '{}');
  assert.throws(
    () => execFileSync(process.execPath, [join(here, 'gen.ts'), join(dir, 'empty')], { encoding: 'utf8', stdio: 'pipe' }),
    /no nodes_/,
  );
});
