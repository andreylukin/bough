import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { api, type Scope } from "../api";
import { ChangesBody, ChangesPage, type useChanges } from "../changes";
import { workRow } from "./fixtures";

// The changes review: session edits and the working tree, with one file's
// patch in the popover and every file as a card on the full page.

const row = workRow({ id: "w-chg", title: "Split the serve API by resource", status: "idle", live: false });

const patch = `diff --git a/go/internal/serve/api.go b/go/internal/serve/api.go
index 3a1f2c0..9b7d4e1 100644
--- a/go/internal/serve/api.go
+++ b/go/internal/serve/api.go
@@ -84,9 +84,11 @@ func (s *Server) sessions(w http.ResponseWriter, r *http.Request) {
 	rows, err := s.store.List(r.Context())
 	if err != nil {
-		http.Error(w, err.Error(), 500)
+		writeError(w, http.StatusInternalServerError, err)
 		return
 	}
-	writeJSON(w, 200, rows)
+	sort.Slice(rows, func(i, j int) bool { return rows[i].Modified.After(rows[j].Modified) })
+	writeJSON(w, http.StatusOK, rows)
+	s.metrics.Sessions.Set(float64(len(rows)))
 }

 func (s *Server) session(w http.ResponseWriter, r *http.Request) {
@@ -140,6 +142,5 @@ func (s *Server) diff(w http.ResponseWriter, r *http.Request) {
 	path := r.URL.Query().Get("path")
-	if path == "" {
-		http.Error(w, "path required", 400)
-	}
+	if path == "" { writeError(w, http.StatusBadRequest, errNoPath); return }
 	out, err := git.Diff(r.Context(), s.cwd, path)
`;

const newFile = `diff --git a/go/internal/serve/errors.go b/go/internal/serve/errors.go
new file mode 100644
--- /dev/null
+++ b/go/internal/serve/errors.go
@@ -0,0 +1,9 @@
+package serve
+
+import "net/http"
+
+var errNoPath = errString("path required")
+
+func writeError(w http.ResponseWriter, code int, err error) {
+	writeJSON(w, code, map[string]string{"error": err.Error()})
+}
`;

const files = [
  { path: "/w/bough/go/internal/serve/api.go", add: 4, del: 4, patch: true },
  { path: "/w/bough/go/internal/serve/errors.go", add: 9, del: 0, new: true, patch: true },
  { path: "/w/bough/go/internal/serve/web/src/changes.tsx", add: 212, del: 38, patch: true },
  { path: "/w/bough/go/internal/serve/web/dist/logo.png", add: -1, del: -1, patch: true },
  { path: "/w/bough/go/internal/serve/api_test.go", add: 61, del: 12, patch: true },
];
const many = [...files, ...Array.from({ length: 6 }, (_, i) => ({ path: `/w/bough/go/internal/serve/routes/r${i + 1}.go`, add: 10 + i, del: i, patch: true }))];

const mock = (list: typeof files, recorded = true) => {
  const read = { files: recorded ? list : list.map((f) => ({ ...f, patch: false })), repo: true, failed: false, at: Date.now() };
  api.diff = (_id: string, path: string) => Promise.resolve(path.endsWith("errors.go") ? newFile : patch);
  return { session: read, tree: read, retry: () => {} } as ReturnType<typeof useChanges>;
};

function Popover({ list, recorded }: { list: typeof files; recorded?: boolean }) {
  const [scope, setScope] = useState<Scope>("session");
  return <div className="chg-pop" style={{ width: "min(560px,100%)", padding: 12 }}><ChangesBody row={row} data={mock(list, recorded)} scope={scope} onScope={setScope} /></div>;
}

const meta: Meta = {
  title: "Changes",
  decorators: [(Story) => <div className="app" style={{ height: "100vh", overflow: "auto" }}><Story /></div>],
};
export default meta;
type S = StoryObj;

/** The header popover: a file list, one patch under it. */
export const PopoverStory: S = { name: "Popover", render: () => <Popover list={files} /> };

/** The full page: summary line, a card per file (a new file, a binary one), the first diff open. */
export const PageMultiFile: S = {
  render: () => {
    const read = mock(files);
    api.edits = () => Promise.resolve({ repo: true, files: read.session.files as never });
    api.changes = () => Promise.resolve({ repo: true, files: read.tree.files as never });
    window.location.hash = "#/s/w-chg/changes?file=" + encodeURIComponent(files[0].path);
    return <ChangesPage row={row} tick={0} onBack={() => {}} />;
  },
};

/** More than eight files: the path filter appears. */
export const PageManyFiles: S = {
  render: () => {
    mock(many);
    api.edits = () => Promise.resolve({ repo: true, files: many as never });
    api.changes = () => Promise.resolve({ repo: true, files: many as never });
    window.location.hash = "";
    return <ChangesPage row={row} tick={0} onBack={() => {}} />;
  },
};

/** No checkpoint was taken: every file says so and nothing opens. */
export const PatchNotRecorded: S = { render: () => <Popover list={files.slice(0, 3)} recorded={false} /> };

export const Phone: S = { ...PageMultiFile, globals: { viewport: { value: "mobile1" } } };
