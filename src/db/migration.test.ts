/**
 * Startup migration for the guest-owned workspace-column semantic change: legacy
 * rows whose workspace points at a per-session worktree under the workspaces
 * root are rewritten to the session's origin dir — but only when the VM backend
 * is active (host-worktree mode still runs sessions in those worktrees).
 */
import { assertEquals } from "jsr:@std/assert@1";
import { join } from "node:path";
import { Db } from "./db.ts";

function withEnv(vars: Record<string, string | undefined>, fn: () => void): void {
  const prev = new Map<string, string | undefined>();
  for (const [k, v] of Object.entries(vars)) {
    prev.set(k, Deno.env.get(k));
    if (v === undefined) Deno.env.delete(k);
    else Deno.env.set(k, v);
  }
  try {
    fn();
  } finally {
    for (const [k, v] of prev) {
      if (v === undefined) Deno.env.delete(k);
      else Deno.env.set(k, v);
    }
  }
}

function seed(path: string, root: string): void {
  const db = new Db(path); // constructed WITHOUT the VM flag — no migration yet
  db.createSession({
    id: "legacy",
    parentId: null,
    title: "legacy",
    kind: "root",
    createdAt: 1,
    workspace: join(root, "legacy"),
    originDir: "/repos/proj",
  });
  db.createSession({
    id: "outside",
    parentId: null,
    title: "outside",
    kind: "root",
    createdAt: 2,
    workspace: "/repos/other",
    originDir: "/repos/other",
  });
  db.createSession({
    id: "no-origin",
    parentId: null,
    title: "no-origin",
    kind: "root",
    createdAt: 3,
    workspace: join(root, "no-origin"),
  });
  db.close();
}

Deno.test("startup migration rewrites legacy worktree workspace rows to origin_dir (VM mode)", () => {
  const tmp = Deno.makeTempDirSync({ prefix: "bough-db-migration-" });
  const root = join(tmp, "workspaces");
  const path = join(tmp, "bough.db");
  withEnv({ BOUGH_SANDBOX_VM: undefined, BOUGH_SUBAGENT_BASE: root }, () => seed(path, root));
  withEnv({ BOUGH_SANDBOX_VM: "1", BOUGH_SUBAGENT_BASE: root }, () => {
    const db = new Db(path);
    // Under the workspaces root + has an origin dir → rewritten to the origin.
    assertEquals(db.getSession("legacy")?.workspace, "/repos/proj");
    // The rewritten ids are surfaced so main.ts can retire the leftover
    // worktrees still squatting on the mirror paths (one-shot migration).
    assertEquals(db.migratedLegacyWorkspaces, ["legacy"]);
    // Not under the workspaces root → untouched.
    assertEquals(db.getSession("outside")?.workspace, "/repos/other");
    // No origin dir recorded → nothing sane to rewrite to; untouched.
    assertEquals(db.getSession("no-origin")?.workspace, join(root, "no-origin"));
    db.close();
  });
  Deno.removeSync(tmp, { recursive: true });
});

Deno.test("startup migration is a no-op without the VM backend", () => {
  const tmp = Deno.makeTempDirSync({ prefix: "bough-db-migration-" });
  const root = join(tmp, "workspaces");
  const path = join(tmp, "bough.db");
  withEnv({ BOUGH_SANDBOX_VM: undefined, BOUGH_SUBAGENT_BASE: root }, () => {
    seed(path, root);
    const db = new Db(path); // reopen, still host-worktree mode
    assertEquals(db.getSession("legacy")?.workspace, join(root, "legacy"));
    db.close();
  });
  Deno.removeSync(tmp, { recursive: true });
});
