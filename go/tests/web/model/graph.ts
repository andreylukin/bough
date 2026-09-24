// Reads a fizz run dir's state graph and emits a transition-covering set of
// paths: every link at least once, each reached through a shortest prefix
// from the initial state. The graph is the protobuf pair described in
// go/tests/model/GRAPH.md, decoded from the wire format here so the test
// toolchain needs no protobuf dependency. go/tests/model/tracecheck reads
// the same files and replays the `trace` of each path.
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

export interface Node { index: number; name: string; state: Record<string, unknown> }
// type is "action" for an action link; fork links inside an action (e.g.
// "Any:locked=True" out of an intermediate node) have no type.
export interface Link { index: number; src: number; dest: number; name: string; type: string }
export interface Graph { nodes: Node[]; links: Link[] }

// One abstract step: an action and the settled state after it (fork links
// are folded into the action that made them). The first step is "Init".
export interface Step { action: string; state: Record<string, unknown> }
export interface Path { target: number; links: number[]; trace: Step[] }
export interface Tally { covered: number; total: number; uncovered: number[] }
export interface Output {
  paths: Path[];
  coverage: { states: Tally; transitions: Tally };
}

type Field = { num: number; wire: number; int: bigint; bytes: Uint8Array };

function* fields(b: Uint8Array): Generator<Field> {
  let i = 0;
  const varint = (): bigint => {
    let v = 0n, shift = 0n;
    for (;;) {
      if (i >= b.length) throw new Error('truncated varint');
      const c = b[i++];
      v |= BigInt(c & 0x7f) << shift;
      if (!(c & 0x80)) return v;
      shift += 7n;
    }
  };
  while (i < b.length) {
    const tag = varint();
    const num = Number(tag >> 3n), wire = Number(tag & 7n);
    const empty = new Uint8Array(0);
    if (wire === 0) yield { num, wire, int: varint(), bytes: empty };
    else if (wire === 1) { i += 8; yield { num, wire, int: 0n, bytes: empty }; }
    else if (wire === 5) { i += 4; yield { num, wire, int: 0n, bytes: empty }; }
    else if (wire === 2) {
      const n = Number(varint());
      if (i + n > b.length) throw new Error('truncated field');
      yield { num, wire, int: 0n, bytes: b.subarray(i, i + n) };
      i += n;
    } else throw new Error(`unsupported wire type ${wire}`);
  }
}

const utf8 = new TextDecoder();

// Shards are read in name order: node indices are global across them.
export function loadGraph(dir: string): Graph {
  const files = readdirSync(dir).sort();
  const nodeFiles = files.filter((f) => /^nodes_\d+_of_\d+\.pb$/.test(f));
  const linkFiles = files.filter((f) => /^adjacency_lists_\d+_of_\d+\.pb$/.test(f));
  if (!nodeFiles.length || !linkFiles.length) {
    throw new Error(`${dir} has no nodes_*/adjacency_lists_* shards (a failing fizz run writes only the error trace)`);
  }
  const g: Graph = { nodes: [], links: [] };
  for (const f of nodeFiles) {
    // message Nodes { repeated string json = 1; }
    for (const fl of fields(readFileSync(join(dir, f)))) {
      if (fl.num !== 1 || fl.wire !== 2) continue;
      const n = JSON.parse(utf8.decode(fl.bytes));
      g.nodes.push({ index: g.nodes.length, name: n.name, state: n.state ?? {} });
    }
  }
  for (const f of linkFiles) {
    // message Links { int64 total_nodes = 1; repeated Link links = 2; }
    // proto3 omits zeros, so a link out of node 0 carries no src field.
    for (const fl of fields(readFileSync(join(dir, f)))) {
      if (fl.num !== 2 || fl.wire !== 2) continue;
      const l: Link = { index: g.links.length, src: 0, dest: 0, name: '', type: '' };
      for (const x of fields(fl.bytes)) {
        if (x.num === 1 && x.wire === 0) l.src = Number(x.int);
        else if (x.num === 2 && x.wire === 0) l.dest = Number(x.int);
        else if (x.num === 3 && x.wire === 2) l.name = utf8.decode(x.bytes);
        else if (x.num === 9 && x.wire === 2) l.type = utf8.decode(x.bytes);
      }
      if (!(l.src < g.nodes.length && l.dest < g.nodes.length)) {
        throw new Error(`${f}: link ${l.index} (${l.name}) points outside ${g.nodes.length} nodes`);
      }
      g.links.push(l);
    }
  }
  return g;
}

// BFS over links in file order, so the prefix chosen for each node is the
// shortest one and the same on every run.
function bfs(g: Graph, from: number[], follow: (l: Link) => boolean): Map<number, number | null> {
  const parent = new Map<number, number | null>(from.map((n) => [n, null]));
  const out = outLinks(g);
  for (let frontier = from; frontier.length; ) {
    const next: number[] = [];
    for (const n of frontier) for (const l of out[n]) {
      if (follow(l) && !parent.has(l.dest)) { parent.set(l.dest, l.index); next.push(l.dest); }
    }
    frontier = next;
  }
  return parent;
}

function outLinks(g: Graph): Link[][] {
  const out: Link[][] = g.nodes.map(() => []);
  for (const l of g.links) out[l.src].push(l);
  return out;
}

function chain(g: Graph, parent: Map<number, number | null>, n: number): number[] {
  const links: number[] = [];
  for (let p = parent.get(n); p != null; p = parent.get(g.links[p].src)) links.unshift(p);
  return links;
}

// A path that stops on an intermediate node (inside an action, before its
// fork resolves) is not a state an implementation can be observed in, so
// it is extended along fork links to the nearest settled node.
function settleTail(g: Graph, n: number): number[] {
  if (g.nodes[n].name === 'yield') return [];
  const parent = bfs(g, [n], (l) => l.type !== 'action');
  for (const [m] of parent) if (g.nodes[m].name === 'yield') return chain(g, parent, m);
  return [];
}

function trace(g: Graph, links: number[]): Step[] {
  const steps: Step[] = [{ action: 'Init', state: g.nodes[0].state }];
  for (const i of links) {
    const l = g.links[i];
    if (l.type === 'action') steps.push({ action: l.name, state: g.nodes[l.dest].state });
    else steps[steps.length - 1].state = g.nodes[l.dest].state;
  }
  return steps;
}

export function generate(g: Graph): Output {
  const parent = bfs(g, [0], () => true);
  const depth = (n: number) => chain(g, parent, n).length;
  // Deepest targets first: their prefixes cover the shallow links, so the
  // shallow ones rarely need a path of their own. A target's own prefix is
  // always the BFS one, so each link is reached as early as it can be.
  const order = g.links
    .filter((l) => parent.has(l.src))
    .sort((a, b) => depth(b.src) - depth(a.src) || a.index - b.index);
  const covered = new Set<number>();
  const paths: Path[] = [];
  for (const l of order) {
    if (covered.has(l.index)) continue;
    const links = [...chain(g, parent, l.src), l.index, ...settleTail(g, l.dest)];
    links.forEach((i) => covered.add(i));
    paths.push({ target: l.index, links, trace: trace(g, links) });
  }
  const seen = new Set<number>([0, ...[...covered].map((i) => g.links[i].dest)]);
  const tally = (total: number, has: (i: number) => boolean): Tally => {
    const uncovered = [...Array(total).keys()].filter((i) => !has(i));
    return { covered: total - uncovered.length, total, uncovered };
  };
  return {
    paths,
    coverage: {
      states: tally(g.nodes.length, (i) => seen.has(i)),
      transitions: tally(g.links.length, (i) => covered.has(i)),
    },
  };
}
