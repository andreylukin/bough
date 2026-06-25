export async function jget(path) {
  const r = await fetch(path);
  if (!r.ok) throw new Error(`GET ${path} → ${r.status}`);
  return r.json();
}

export async function jpost(path, body) {
  const r = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body || {}),
  });
  if (!r.ok) throw new Error(`POST ${path} → ${r.status}: ${await r.text()}`);
  const t = await r.text();
  return t ? JSON.parse(t) : {};
}

export async function jdel(path) {
  const r = await fetch(path, { method: "DELETE" });
  if (!r.ok) throw new Error(`DELETE ${path} → ${r.status}`);
  return {};
}

export const api = {
  config: () => jget("/config"),
  sessions: () => jget("/sessions"),
  createSession: (project) => jpost("/session", { project }),
  tree: (id) => jget(`/session/${id}`),
  run: (id) => jget(`/session/${id}/run`),
  startRun: (id, content, review) => jpost(`/session/${id}/run`, { content, review }),
  control: (id, decision, message) => jpost(`/session/${id}/control`, { decision, message: message || "" }),
  stop: (id) => jpost(`/session/${id}/control`, { decision: "stop" }),
  fork: (id, entry_id) => jpost(`/session/${id}/fork`, { entry_id }),
  graft: (id, section_root, onto) => jpost(`/session/${id}/graft`, { section_root, onto }),
  label: (id, entry_id, label) => jpost(`/session/${id}/label`, { entry_id, label }),
  adopt: (id, entry_id) => jpost(`/session/${id}/adopt`, { entry_id }),
  subagents: (id) => jget(`/session/${id}/subagents`),
  diff: (id) => jget(`/session/${id}/diff`),
  files: (id) => jget(`/session/${id}/files`),
  groupsCatalog: () => jget("/groups"),
  groupDetail: (name) => jget(`/groups/${name}`),
  setGroups: (id, groups) => jpost(`/session/${id}/groups`, { groups }),
  packs: () => jget("/packs"),
  savePack: (pack) => jpost("/packs", pack),
  deletePack: (name) => jdel(`/packs/${encodeURIComponent(name)}`),
  applyPacks: (id, names) => jpost(`/session/${id}/packs`, { names }),
  draftPack: (description) => jpost("/packs/draft", { description }),
};

// ---- helpers -------------------------------------------------------------
