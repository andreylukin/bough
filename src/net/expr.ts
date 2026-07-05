/**
 * Condition expressions — the language rule `condition`s are written in (config.ts
 * `rules`, evaluated by policy.ts decide()). A deliberately small CEL subset, matching
 * how upstream Claw Patrol rules read, hand-rolled so the gate has no dependency and
 * evaluation is deterministic:
 *
 *   http.method == 'POST' && http.path == '/graphql'
 *   http.body_json.query.startsWith('mutation')
 *   k8s.resource in ['pods/exec', 'pods/attach']
 *   has(http.body_json.archived) && http.body_json.archived == true
 *   http.headers['content-type'].contains('json')
 *
 * Supported: string/number/bool literals, list literals, dotted identifiers rooted in
 * the facet env, indexing, `==` `!=` `in` `&&` `||` `!`, parentheses, the string
 * methods startsWith/endsWith/contains/matches, and has(path). No arithmetic, no
 * comprehensions.
 *
 * Failure semantics mirror CEL's, which is what makes rules safe to write:
 *   - compile() throws on a malformed expression, so a bad rule is rejected at edit /
 *     load time (config.ts validates every condition inside the zod parse) and never
 *     reaches the gate;
 *   - evaluation errors (unknown identifier, selecting a field the payload doesn't
 *     carry, method on a non-string, non-boolean result) THROW — decide() catches and
 *     fails closed (deny), so an unevaluable condition can never open the gate. Guard
 *     optional fields with has().
 *   - && and || short-circuit, so `http.path == '/graphql' && http.body_json...` only
 *     touches body_json for requests the left side already matched.
 */

/** A thrown condition failure — compile-time (parse) or eval-time (bad env access). */
export class ExprError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ExprError";
  }
}

/** The env a condition evaluates against: facet name → fields (see policy.ts ruleEnv). */
export type ExprEnv = Record<string, unknown>;

export interface CompiledExpr {
  src: string;
  /** Evaluate to a boolean; throws ExprError when the condition can't be evaluated. */
  test(env: ExprEnv): boolean;
}

// ---- tokenizer ---------------------------------------------------------------

type Token =
  | { t: "str"; v: string }
  | { t: "num"; v: number }
  | { t: "ident"; v: string }
  | { t: "punct"; v: string };

const PUNCT = ["==", "!=", "&&", "||", "!", "(", ")", "[", "]", ",", "."];

function tokenize(src: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (c === " " || c === "\t" || c === "\n" || c === "\r") {
      i++;
      continue;
    }
    if (c === "'" || c === '"') {
      let j = i + 1;
      let v = "";
      while (j < src.length && src[j] !== c) {
        if (src[j] === "\\" && j + 1 < src.length) {
          v += src[j + 1];
          j += 2;
        } else {
          v += src[j];
          j++;
        }
      }
      if (j >= src.length) throw new ExprError(`unterminated string at ${i}`);
      out.push({ t: "str", v });
      i = j + 1;
      continue;
    }
    if (c >= "0" && c <= "9") {
      let j = i;
      while (j < src.length && /[0-9.]/.test(src[j])) j++;
      const raw = src.slice(i, j);
      const v = Number(raw);
      if (Number.isNaN(v)) throw new ExprError(`bad number "${raw}"`);
      out.push({ t: "num", v });
      i = j;
      continue;
    }
    if (/[A-Za-z_]/.test(c)) {
      let j = i;
      while (j < src.length && /[A-Za-z0-9_]/.test(src[j])) j++;
      out.push({ t: "ident", v: src.slice(i, j) });
      i = j;
      continue;
    }
    const two = src.slice(i, i + 2);
    if (PUNCT.includes(two)) {
      out.push({ t: "punct", v: two });
      i += 2;
      continue;
    }
    if (PUNCT.includes(c)) {
      out.push({ t: "punct", v: c });
      i++;
      continue;
    }
    throw new ExprError(`unexpected character "${c}" at ${i}`);
  }
  return out;
}

// ---- parser ------------------------------------------------------------------

type EvalFn = (env: ExprEnv) => unknown;

/** A parsed node; member/index nodes carry {parent, key} so has() can probe them. */
interface Node {
  eval: EvalFn;
  /** Set on `.field` / `[key]` access nodes — what has() needs to test existence. */
  access?: { parent: Node; key: EvalFn };
}

const STRING_METHODS = new Set(["startsWith", "endsWith", "contains", "matches"]);

function isPlainValue(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

class Parser {
  #toks: Token[];
  #pos = 0;

  constructor(src: string) {
    this.#toks = tokenize(src);
  }

  parse(): Node {
    const node = this.#or();
    if (this.#pos < this.#toks.length) {
      throw new ExprError(`unexpected trailing token "${this.#describe(this.#toks[this.#pos])}"`);
    }
    return node;
  }

  #describe(tok: Token): string {
    return tok.t === "str" ? `'${tok.v}'` : String(tok.v);
  }

  #peek(v?: string): Token | undefined {
    const tok = this.#toks[this.#pos];
    if (!tok) return undefined;
    if (v !== undefined && !(tok.t === "punct" && tok.v === v)) return undefined;
    return tok;
  }

  #eat(v: string): boolean {
    if (this.#peek(v)) {
      this.#pos++;
      return true;
    }
    return false;
  }

  #expect(v: string): void {
    if (!this.#eat(v)) {
      const at = this.#toks[this.#pos];
      throw new ExprError(`expected "${v}"${at ? ` before "${this.#describe(at)}"` : " at end"}`);
    }
  }

  #or(): Node {
    let left = this.#and();
    while (this.#eat("||")) {
      const l = left;
      const r = this.#and();
      left = { eval: (env) => truthy(l.eval(env), "||") || truthy(r.eval(env), "||") };
    }
    return left;
  }

  #and(): Node {
    let left = this.#not();
    while (this.#eat("&&")) {
      const l = left;
      const r = this.#not();
      left = { eval: (env) => truthy(l.eval(env), "&&") && truthy(r.eval(env), "&&") };
    }
    return left;
  }

  #not(): Node {
    if (this.#eat("!")) {
      const inner = this.#not();
      return { eval: (env) => !truthy(inner.eval(env), "!") };
    }
    return this.#rel();
  }

  #rel(): Node {
    const left = this.#postfix();
    if (this.#eat("==")) {
      const r = this.#postfix();
      return { eval: (env) => left.eval(env) === r.eval(env) };
    }
    if (this.#eat("!=")) {
      const r = this.#postfix();
      return { eval: (env) => left.eval(env) !== r.eval(env) };
    }
    const tok = this.#toks[this.#pos];
    if (tok?.t === "ident" && tok.v === "in") {
      this.#pos++;
      const r = this.#postfix();
      return {
        eval: (env) => {
          const needle = left.eval(env);
          const hay = r.eval(env);
          if (Array.isArray(hay)) return hay.includes(needle);
          if (isPlainValue(hay)) return typeof needle === "string" && needle in hay;
          throw new ExprError(`"in" needs a list or map on the right`);
        },
      };
    }
    return left;
  }

  #postfix(): Node {
    let node = this.#primary();
    for (;;) {
      if (this.#eat(".")) {
        const tok = this.#toks[this.#pos];
        if (tok?.t !== "ident") throw new ExprError(`expected a field name after "."`);
        this.#pos++;
        const name = tok.v;
        if (this.#eat("(")) {
          node = this.#method(node, name);
        } else {
          node = this.#member(node, () => name, `.${name}`);
        }
        continue;
      }
      if (this.#eat("[")) {
        const key = this.#or();
        this.#expect("]");
        node = this.#member(node, (env) => key.eval(env), "[...]");
        continue;
      }
      return node;
    }
  }

  /** `.field` / `[key]` access: missing field is an ERROR (CEL semantics) — guard with has(). */
  #member(parent: Node, key: EvalFn, label: string): Node {
    return {
      access: { parent, key },
      eval: (env) => {
        const obj = parent.eval(env);
        const k = key(env);
        if (Array.isArray(obj)) {
          if (typeof k !== "number") throw new ExprError(`list index must be a number`);
          if (k < 0 || k >= obj.length) throw new ExprError(`list index ${k} out of range`);
          return obj[k];
        }
        if (!isPlainValue(obj)) throw new ExprError(`cannot select ${label} on a non-map value`);
        if (typeof k !== "string") throw new ExprError(`map key must be a string`);
        if (!(k in obj)) throw new ExprError(`no such field: ${k}`);
        return obj[k];
      },
    };
  }

  #method(recv: Node, name: string): Node {
    if (!STRING_METHODS.has(name)) throw new ExprError(`unknown method .${name}()`);
    const arg = this.#or();
    this.#expect(")");
    return {
      eval: (env) => {
        const s = recv.eval(env);
        const a = arg.eval(env);
        if (typeof s !== "string") throw new ExprError(`.${name}() needs a string receiver`);
        if (typeof a !== "string") throw new ExprError(`.${name}() needs a string argument`);
        switch (name) {
          case "startsWith":
            return s.startsWith(a);
          case "endsWith":
            return s.endsWith(a);
          case "contains":
            return s.includes(a);
          default: {
            try {
              return new RegExp(a).test(s);
            } catch {
              throw new ExprError(`invalid regex in .matches(): ${a}`);
            }
          }
        }
      },
    };
  }

  #primary(): Node {
    const tok = this.#toks[this.#pos];
    if (!tok) throw new ExprError("unexpected end of expression");
    if (tok.t === "str" || tok.t === "num") {
      this.#pos++;
      const v = tok.v;
      return { eval: () => v };
    }
    if (tok.t === "ident") {
      if (tok.v === "true" || tok.v === "false") {
        this.#pos++;
        const v = tok.v === "true";
        return { eval: () => v };
      }
      if (tok.v === "has") {
        this.#pos++;
        this.#expect("(");
        const inner = this.#postfix();
        this.#expect(")");
        if (!inner.access) throw new ExprError("has() needs a field selection, e.g. has(a.b)");
        // A true guard, one notch friendlier than CEL: has(a.b.c) is false whenever
        // the path can't resolve — the facet isn't in this request's env, body_json
        // isn't JSON, an intermediate field is missing — instead of erroring. This is
        // what lets one rule set span requests with different facets.
        return {
          eval: (env) => {
            try {
              inner.eval(env);
              return true;
            } catch (e) {
              if (e instanceof ExprError) return false;
              throw e;
            }
          },
        };
      }
      this.#pos++;
      const name = tok.v;
      return {
        eval: (env) => {
          if (!(name in env)) throw new ExprError(`unknown identifier: ${name}`);
          return env[name];
        },
      };
    }
    if (tok.t === "punct" && tok.v === "[") {
      this.#pos++;
      const items: Node[] = [];
      if (!this.#peek("]")) {
        do {
          items.push(this.#or());
        } while (this.#eat(","));
      }
      this.#expect("]");
      return { eval: (env) => items.map((n) => n.eval(env)) };
    }
    if (tok.t === "punct" && tok.v === "(") {
      this.#pos++;
      const inner = this.#or();
      this.#expect(")");
      return inner;
    }
    throw new ExprError(`unexpected token "${this.#describe(tok)}"`);
  }
}

function truthy(v: unknown, op: string): boolean {
  if (typeof v !== "boolean") throw new ExprError(`${op} needs boolean operands`);
  return v;
}

/** Compile a condition. Throws ExprError on a malformed expression — validate at edit time. */
export function compile(src: string): CompiledExpr {
  const node = new Parser(src).parse();
  return {
    src,
    test(env: ExprEnv): boolean {
      const out = node.eval(env);
      if (typeof out !== "boolean") {
        throw new ExprError(`condition evaluated to ${typeof out}, expected boolean`);
      }
      return out;
    },
  };
}
