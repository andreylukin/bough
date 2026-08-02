/**
 * Stage two of clustering: group logtypes that masking could not make identical.
 *
 * WHAT IS LEFT FOR THIS STAGE. `mask.ts` deletes everything that LOOKS like a
 * value, which handles the majority of variability. What survives is variability
 * that looks like language: a hostname, a queue name, an exception class, an error
 * string copied out of errno. Those are as variable as any integer and no regex can
 * say so, because `db-primary` and `connection refused` are the same shape as the
 * fixed words around them. Deciding they are variable requires seeing many lines
 * and noticing that one position keeps changing while its neighbours do not — which
 * is what a clustering pass is for.
 *
 * THE ALGORITHM (Drain, He et al., ICWS 2017). A fixed-depth tree whose first level
 * is token COUNT and whose next few levels are the leading tokens. Lines that reach
 * the same leaf are compared to the handful of templates already there, and one is
 * chosen if it agrees on enough positions. Where the winner and the new line
 * disagree, the template gains a wildcard.
 *
 * WHY A TREE RATHER THAN COMPARING EVERYTHING TO EVERYTHING. The similarity test is
 * O(tokens) and the naive version runs it against every known template for every
 * line, which is quadratic in a quantity that grows without bound. The tree makes
 * the candidate set a leaf's worth of templates — a few — regardless of how many
 * exist in total. Token count and leading tokens are the prefix chosen because they
 * are cheap, and because two lines that disagree on either are never the same log
 * statement anyway, so no true match is lost by refusing to compare them.
 *
 * ONE PASS, NO SECOND LOOK. A cluster is created or updated the moment a line
 * arrives and is never revisited. This is what lets the whole pipeline stream —
 * memory holds clusters, never lines — and it has one consequence worth stating
 * plainly: templates depend on ARRIVAL ORDER. Two files with the same lines shuffled
 * can produce slightly different wildcard placement. The counts and statistics are
 * unaffected; only the rendered template can differ, and only in which positions
 * generalized first.
 *
 * BOUNDED, AND HONEST WHEN THE BOUND BINDS. Clusters are capped and evicted
 * least-recently-used. Eviction loses a real cluster's real count, so it is
 * reported rather than absorbed — `truncated` reaches the header, because a reader
 * comparing two runs deserves to know when the second measured something different.
 *
 * Pure: no clock, no filesystem, no randomness.
 */

/** A group of lines the tree considers the same statement. */
export interface Cluster {
  id: number;
  /** Template tokens. A position that generalized holds `WILDCARD`. */
  tokens: string[];
  count: number;
}

/** The token that marks a position whose value varies in a way masking could not type. */
export const WILDCARD = "<*>";

/** Tunables. Defaults are the paper's, except the cap, which is ours. */
export interface DrainOptions {
  /**
   * Fraction of positions that must agree for a line to join a template.
   *
   * 0.4 is the paper's default and it survives contact with real logs. Higher and
   * near-identical statements refuse to merge, leaving forty templates that differ
   * in one word; lower and unrelated statements of the same length collapse into a
   * template that is mostly wildcards and says nothing.
   */
  threshold?: number;
  /**
   * Total tree depth, counted the paper's way: root, the token-count level, the
   * token levels, and the leaf. So the number of LEADING TOKENS actually indexed is
   * `depth - 2`, and the default of 4 indexes two of them.
   *
   * This off-by-two is worth stating because getting it wrong is silent and
   * expensive. Every token consumed as an index is a token that can never
   * generalize — it selects a subtree, so two lines differing there are never even
   * compared. Indexing four tokens instead of two means `connect to db-primary
   * failed` and `connect to db-replica failed` land in different leaves and stay
   * separate forever, which looks exactly like a threshold that is too strict and
   * cannot be fixed by tuning one.
   */
  depth?: number;
  /** Distinct children before a node stops splitting and funnels the rest to one wildcard child. */
  maxChildren?: number;
  /** Clusters held before least-recently-used eviction begins. */
  maxClusters?: number;
  /** Called with a cluster the cap forced out, so a caller can drop its statistics too. */
  onEvict?: (cluster: Cluster) => void;
}

interface Node {
  children: Map<string, Node>;
  clusters: Cluster[];
}

function node(): Node {
  return { children: new Map(), clusters: [] };
}

/**
 * A token is used as a tree index only if it is stable enough to index on.
 *
 * Two things disqualify a token, and BOTH are needed:
 *
 *   - IT CONTAINS A DIGIT. `worker-3` is a value wearing a word's clothes, and
 *     indexing on it gives every worker its own subtree — one statement fragmented
 *     into as many clusters as there are workers. This is the paper's own rule.
 *   - IT CONTAINS A PLACEHOLDER. This is the rule the paper does not need and this
 *     pipeline does, because masking runs first. A masked token like `[<hex>]` has
 *     no digit and would index literally, while the SAME statement's other lines —
 *     whose id happened to be too short for the masker to type — index as
 *     `[a1b2c3d]`, which does contain a digit and keys as wildcard. The two land in
 *     different leaves, are never compared, and one log statement becomes two
 *     patterns whose statistics each describe half the data. Masking makes a token
 *     MORE variable, not less, so a placeholder must disqualify a token exactly the
 *     way a digit does.
 *
 * Precision is not lost by being permissive here: the index only decides which
 * templates are worth comparing, and the similarity test at the leaf still refuses
 * to merge lines that disagree.
 */
function indexKey(token: string): string {
  return /\d/.test(token) || token.includes("<") ? WILDCARD : token;
}

export class Drain {
  private readonly root = new Map<number, Node>();
  private readonly threshold: number;
  /** Leading tokens used as tree index: `depth - 2`, per the paper's depth convention. */
  private readonly indexTokens: number;
  private readonly maxChildren: number;
  private readonly maxClusters: number;
  private readonly onEvict?: (cluster: Cluster) => void;
  /** Insertion-ordered, which is what makes it usable as the LRU queue. */
  private readonly byId = new Map<number, Cluster>();
  private nextId = 1;
  private evicted = 0;

  constructor(opts: DrainOptions = {}) {
    this.threshold = opts.threshold ?? 0.4;
    // `depth` is the whole tree; `indexTokens` is the part this class walks. Floor
    // of one so a caller passing `depth: 2` still gets a usable tree rather than a
    // single leaf holding every line of a given length.
    this.indexTokens = Math.max(1, (opts.depth ?? 4) - 2);
    this.maxChildren = opts.maxChildren ?? 100;
    this.maxClusters = opts.maxClusters ?? 10000;
    if (opts.onEvict) this.onEvict = opts.onEvict;
  }

  /** True once the cap forced a cluster out and counts stopped being complete. */
  get truncated(): boolean {
    return this.evicted > 0;
  }

  /** Every live cluster, most frequent first. */
  clusters(): Cluster[] {
    return [...this.byId.values()].sort((a, b) => b.count - a.count);
  }

  /**
   * Add one masked line's tokens; return the cluster it belongs to.
   *
   * The returned object is the live cluster, so a caller may hold it — but its
   * `tokens` are mutated in place as the template generalizes, which is exactly why
   * statistics must be keyed on `id` and on token POSITION rather than on the token
   * text at the time of insertion.
   */
  add(tokens: string[]): Cluster {
    const leaf = this.descend(tokens);
    const match = this.bestMatch(leaf.clusters, tokens);
    if (match) {
      // Generalize in place: positions that disagree become wildcards, permanently.
      // A template only ever loses specificity, which is what makes one pass enough
      // — a position that has varied once will vary again.
      for (let i = 0; i < match.tokens.length; i++) {
        if (match.tokens[i] !== tokens[i] && match.tokens[i] !== WILDCARD) {
          match.tokens[i] = WILDCARD;
        }
      }
      match.count++;
      this.touch(match);
      return match;
    }
    const created: Cluster = { id: this.nextId++, tokens: [...tokens], count: 1 };
    leaf.clusters.push(created);
    this.byId.set(created.id, created);
    this.evictIfNeeded(leaf);
    return created;
  }

  /** Walk to the leaf for these tokens, creating nodes as needed. */
  private descend(tokens: string[]): Node {
    // Token count first. Two lines of different length are never one statement, so
    // this partition is free precision.
    let current: Node;
    const atLength = this.root.get(tokens.length);
    if (atLength) {
      current = atLength;
    } else {
      current = node();
      this.root.set(tokens.length, current);
    }
    const levels = Math.min(this.indexTokens, tokens.length);
    for (let i = 0; i < levels; i++) {
      const key = indexKey(tokens[i] as string);
      let next: Node | undefined = current.children.get(key);
      if (!next) {
        // Past the fan-out limit, everything new shares one wildcard child rather
        // than growing the tree without bound. A node with ten thousand children is
        // a linear scan wearing a tree's clothes.
        if (current.children.size >= this.maxChildren) {
          next = current.children.get(WILDCARD);
          if (!next) {
            next = node();
            current.children.set(WILDCARD, next);
          }
        } else {
          next = node();
          current.children.set(key, next);
        }
      }
      current = next;
    }
    return current;
  }

  /**
   * The leaf's best template, if any clears the threshold.
   *
   * Similarity is the fraction of positions holding the identical token. A wildcard
   * position counts as agreement for NEITHER side: it is already known to vary, so
   * crediting it would let a template that has generalized twice absorb anything of
   * the right length, and every subsequent line would make it worse. Ties break
   * toward the template with fewer wildcards — the more specific description of the
   * same data is the better one.
   */
  private bestMatch(clusters: Cluster[], tokens: string[]): Cluster | undefined {
    let best: Cluster | undefined;
    let bestSim = -1;
    let bestWildcards = Number.POSITIVE_INFINITY;
    for (const c of clusters) {
      let same = 0;
      let wildcards = 0;
      for (let i = 0; i < c.tokens.length; i++) {
        if (c.tokens[i] === WILDCARD) wildcards++;
        else if (c.tokens[i] === tokens[i]) same++;
      }
      const sim = c.tokens.length === 0 ? 1 : same / c.tokens.length;
      if (sim > bestSim || (sim === bestSim && wildcards < bestWildcards)) {
        best = c;
        bestSim = sim;
        bestWildcards = wildcards;
      }
    }
    return bestSim >= this.threshold ? best : undefined;
  }

  /** Move a cluster to the back of the LRU queue by reinserting it. */
  private touch(c: Cluster): void {
    this.byId.delete(c.id);
    this.byId.set(c.id, c);
  }

  /**
   * Drop the least recently touched cluster once the cap is exceeded.
   *
   * The victim is also spliced out of its leaf. Skipping that would leave a dead
   * template in the candidate list forever — still matched against, still
   * generalizing, but no longer counted anywhere, which corrupts every later line
   * that resembles it rather than merely losing the old one.
   */
  private evictIfNeeded(leafOfNew: Node): void {
    while (this.byId.size > this.maxClusters) {
      const oldest = this.byId.keys().next();
      if (oldest.done) return;
      const victim = this.byId.get(oldest.value) as Cluster;
      this.byId.delete(oldest.value);
      this.evicted++;
      this.unlink(victim, leafOfNew);
      this.onEvict?.(victim);
    }
  }

  /** Remove a cluster from whichever leaf holds it. */
  private unlink(victim: Cluster, hint: Node): void {
    const fromHint = hint.clusters.indexOf(victim);
    if (fromHint >= 0) {
      hint.clusters.splice(fromHint, 1);
      return;
    }
    // The victim is almost never in the leaf we just wrote to, so fall back to a
    // walk. Eviction is rare by construction — it only happens above the cap — so
    // the cost is paid on the pathological input that earned it.
    const stack: Node[] = [...this.root.values()];
    while (stack.length > 0) {
      const n = stack.pop() as Node;
      const at = n.clusters.indexOf(victim);
      if (at >= 0) {
        n.clusters.splice(at, 1);
        return;
      }
      for (const child of n.children.values()) stack.push(child);
    }
  }
}
