// Neutral string helpers shared by the server and the TUI (no theme/terminal deps).

export function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}

/**
 * One-line excerpt of a tool call's input: the first meaningful code line (or
 * compact JSON). A bare tool name ("run_steps") tells the reader nothing about
 * what ran — the transcript folds and the workflow agent view both label calls
 * with this.
 */
export function codeGist(input: unknown, width = 60): string {
  const raw = input as Record<string, unknown> | null | undefined;
  const code = raw && typeof raw.code === "string" ? raw.code : null;
  const src = code ?? (input === undefined ? "" : JSON.stringify(input));
  const line = src.trim().split("\n").map((l) => l.trim())
    .find((l) => l.length > 0 && !l.startsWith("//")) ?? "";
  return clip(line, width);
}

/**
 * Locate a string literal closed by a raw newline. This is THE failure mode for
 * a generated workflow: the supervisor assembles the script inside a template
 * literal, and every `\n` meant for a string in the GENERATED script is consumed
 * by the outer literal, leaving a real newline inside `"..."`. Field case
 * (2026-07-24): a 57-line Rust-rewrite plan died at parse time, 0/0 agents.
 *
 * Written as a scanner rather than a regex because it has to skip comments and
 * template literals, where a raw newline is perfectly legal.
 */
export function unterminatedString(
  src: string,
): { line: number; col: number; text: string; quote: string } | null {
  let line = 1, col = 1, depth = 0;
  for (let i = 0; i < src.length; i++) {
    const c = src[i], next = src[i + 1];
    const bump = () => {
      if (c === "\n") {
        line++;
        col = 1;
      } else col++;
    };
    if (c === "/" && next === "/") {
      while (i < src.length && src[i] !== "\n") i++;
      line++;
      col = 1;
      continue;
    }
    if (c === "/" && next === "*") {
      const end = src.indexOf("*/", i + 2);
      const skipped = src.slice(i, end < 0 ? src.length : end + 2);
      line += (skipped.match(/\n/g) ?? []).length;
      col = 1;
      i = end < 0 ? src.length : end + 1;
      continue;
    }
    // Template literals may span lines legally; walk them (with ${} nesting) so
    // their newlines never look like an unterminated quote.
    if (c === "`") {
      i++;
      for (; i < src.length; i++) {
        if (src[i] === "\\") {
          i++;
          continue;
        }
        if (src[i] === "\n") {
          line++;
          col = 1;
          continue;
        }
        if (src[i] === "$" && src[i + 1] === "{") depth++;
        else if (src[i] === "}" && depth > 0) depth--;
        else if (src[i] === "`" && depth === 0) break;
      }
      col++;
      continue;
    }
    if (c === '"' || c === "'") {
      const startLine = line, startCol = col;
      for (i++; i < src.length; i++) {
        if (src[i] === "\\") {
          i++;
          continue;
        }
        if (src[i] === "\n" || i === src.length - 1 && src[i] !== c) {
          return {
            line: startLine,
            col: startCol,
            text: src.split("\n")[startLine - 1] ?? "",
            quote: c,
          };
        }
        if (src[i] === c) break;
      }
      col++;
      continue;
    }
    bump();
  }
  return null;
}

const AsyncFunctionCtor = Object.getPrototypeOf(async function () {}).constructor as // deno-lint-ignore no-explicit-any
any;

/**
 * Compile-check generated JS BEFORE handing it to a sealed worker, and turn a
 * position-less V8 SyntaxError into something its author can act on.
 *
 * Both VMs (harness/vm.ts for run_steps programs, workflow.ts for workflow
 * scripts) used to discover a parse failure only inside the worker, and reported
 * it as a bare "SyntaxError: Invalid or unexpected token" over ten frames of Deno
 * internals — no line, no column, no source. The author is a model mid-turn; that
 * message gives it nothing to fix. Compiling here is side-effect free: the
 * Function constructor parses, it does not execute, and the code never touches
 * the host's scope.
 *
 * `params` mirrors the worker's own AsyncFunction arity so the two parse alike.
 */
export function checkSyntax(
  code: string,
  params: string[],
  what: string,
): { message: string } | null {
  try {
    new AsyncFunctionCtor(...params, code);
    return null;
  } catch (err) {
    if ((err as Error)?.name !== "SyntaxError") throw err;
    const why = (err as Error).message;
    const hit = unterminatedString(code);
    if (!hit) return { message: `${what} does not parse: ${why}` };
    return {
      message:
        `${what} does not parse: ${why} — line ${hit.line}: a ${
          hit.quote === '"' ? "double" : "single"
        }-quoted string is closed by a real newline.\n  ${hit.line} | ${
          clip(hit.text.trim(), 90)
        }\nIf you built this code inside a template literal, write \\\\n (escaped) for newlines ` +
        `that belong to the GENERATED code's strings — a bare \\n is consumed by the outer literal.`,
    };
  }
}
