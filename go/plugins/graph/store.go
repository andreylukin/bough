// Package graph is bough's long-term memory: a bi-temporal property graph
// in SQLite (docs/graph-memory.md). Entities have deterministic keys,
// edges have a validity window in the world and an observation window in
// bough, every edge cites an episode, and nothing is deleted: a
// contradiction closes a window. Retrieval is FTS5 + embedding cosine +
// reciprocal-rank fusion + hop expansion, and never calls a model.
package graph

import (
	"cmp"
	"context"
	"database/sql"
	"encoding/binary"
	"errors"
	"fmt"
	"math"
	"slices"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

// Schema is the on-disk shape, applied idempotently at Open. The four
// tables are the design doc's; embeddings are a plain blob table and
// FTS5 an external-content-free mirror of the searchable text (entity
// titles and edge claims), because this is thousands of rows, not
// millions: cosine in Go beats a vector extension we cannot load cgo-free.
const Schema = `
CREATE TABLE IF NOT EXISTS entities (
  id    INTEGER PRIMARY KEY,
  kind  TEXT NOT NULL,
  key   TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '',
  attrs TEXT NOT NULL DEFAULT '{}',
  UNIQUE (kind, key)
);
CREATE TABLE IF NOT EXISTS aliases (
  entity_id  INTEGER NOT NULL REFERENCES entities(id),
  source     TEXT NOT NULL,
  foreign_id TEXT NOT NULL,
  url        TEXT,
  PRIMARY KEY (source, foreign_id)
);
CREATE TABLE IF NOT EXISTS episodes (
  id          INTEGER PRIMARY KEY,
  source      TEXT NOT NULL,
  ref         TEXT NOT NULL,
  ingested_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS edges (
  id          INTEGER PRIMARY KEY,
  src         INTEGER NOT NULL REFERENCES entities(id),
  rel         TEXT NOT NULL,
  dst         INTEGER NOT NULL REFERENCES entities(id),
  valid_from  INTEGER NOT NULL,
  valid_to    INTEGER,
  observed_at INTEGER NOT NULL,
  recorded_at INTEGER NOT NULL,
  episode_id  INTEGER NOT NULL REFERENCES episodes(id),
  author      TEXT NOT NULL,
  weight      REAL NOT NULL DEFAULT 1.0,
  claim       TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS edges_src ON edges(src, rel) WHERE valid_to IS NULL;
CREATE INDEX IF NOT EXISTS edges_dst ON edges(dst, rel) WHERE valid_to IS NULL;
CREATE VIRTUAL TABLE IF NOT EXISTS graph_fts USING fts5(kind, ref, text);
CREATE TABLE IF NOT EXISTS embeddings (
  kind  TEXT NOT NULL,
  ref   INTEGER NOT NULL,
  model TEXT NOT NULL,
  vec   BLOB NOT NULL,
  PRIMARY KEY (kind, ref)
);
`

// linkColumns are the entity columns added after the first schema: the
// link truth. url is the canonical link, status the source's current
// state, summary one line about it, updated_at the source's own clock.
// ALTER TABLE has no IF NOT EXISTS, so Open adds what is missing.
var linkColumns = []string{
	"url TEXT NOT NULL DEFAULT ''",
	"status TEXT NOT NULL DEFAULT ''",
	"summary TEXT NOT NULL DEFAULT ''",
	"updated_at INTEGER NOT NULL DEFAULT 0",
}

func migrate(db *sql.DB) error {
	rows, err := db.Query(`PRAGMA table_info(entities)`)
	if err != nil {
		return err
	}
	have := map[string]bool{}
	for rows.Next() {
		var cid int
		var name, typ string
		var notnull, pk int
		var dflt any
		if err := rows.Scan(&cid, &name, &typ, &notnull, &dflt, &pk); err != nil {
			rows.Close()
			return err
		}
		have[name] = true
	}
	rows.Close()
	for _, col := range linkColumns {
		name, _, _ := strings.Cut(col, " ")
		if have[name] {
			continue
		}
		if _, err := db.Exec(`ALTER TABLE entities ADD COLUMN ` + col); err != nil {
			return err
		}
	}
	return nil
}

// Entity is one node.
type Entity struct {
	ID        int64  `json:"id"`
	Kind      string `json:"kind"`
	Key       string `json:"key"`
	Title     string `json:"title,omitempty"`
	Attrs     string `json:"attrs,omitempty"`
	URL       string `json:"url,omitempty"`
	Status    string `json:"status,omitempty"`
	Summary   string `json:"summary,omitempty"`
	UpdatedAt int64  `json:"updated_at,omitempty"`
}

// Edge is one claim: src -rel-> dst, with its two time windows.
type Edge struct {
	ID         int64   `json:"id"`
	Src        Entity  `json:"src"`
	Rel        string  `json:"rel"`
	Dst        Entity  `json:"dst"`
	ValidFrom  int64   `json:"valid_from"`
	ValidTo    *int64  `json:"valid_to,omitempty"`
	ObservedAt int64   `json:"observed_at"`
	RecordedAt int64   `json:"recorded_at"`
	EpisodeID  int64   `json:"episode_id"`
	Author     string  `json:"author"`
	Weight     float64 `json:"weight"`
	Claim      string  `json:"claim,omitempty"`
}

// Episode is provenance: what was ingested, from where, when.
type Episode struct {
	ID         int64  `json:"id"`
	Source     string `json:"source"`
	Ref        string `json:"ref"`
	IngestedAt int64  `json:"ingested_at"`
}

// Store is the graph database.
type Store struct {
	db    *sql.DB
	now   func() time.Time
	embed Embedder // nil: FTS-only retrieval
}

// Open opens (creating) the graph at path and applies the schema.
func Open(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path+"?_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)&_pragma=foreign_keys(ON)")
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1) // one writer, and the FTS/graph tables move together
	if _, err := db.Exec(Schema); err != nil {
		db.Close()
		return nil, fmt.Errorf("graph schema: %w", err)
	}
	if err := migrate(db); err != nil {
		db.Close()
		return nil, fmt.Errorf("graph schema: %w", err)
	}
	return &Store{db: db, now: time.Now}, nil
}

// Close closes the database.
func (s *Store) Close() error { return s.db.Close() }

// SetEmbedder installs the embedder used at write time and for the
// vector half of Search. nil keeps FTS-only.
func (s *Store) SetEmbedder(e Embedder) { s.embed = e }

func (s *Store) unix() int64 { return s.now().Unix() }

// Episode records provenance and returns its id.
func (s *Store) Episode(source, ref string) (int64, error) {
	res, err := s.db.Exec(`INSERT INTO episodes(source, ref, ingested_at) VALUES(?,?,?)`, source, ref, s.unix())
	if err != nil {
		return 0, err
	}
	return res.LastInsertId()
}

// Upsert returns the entity for (kind, key), creating it, and refreshes
// a non-empty title/attrs. The searchable title is mirrored into FTS and
// embedded when an embedder is set.
func (s *Store) Upsert(kind, key, title, attrs string) (Entity, error) {
	if kind == "" || key == "" {
		return Entity{}, errors.New("graph: entity needs kind and key")
	}
	if attrs == "" {
		attrs = "{}"
	}
	_, err := s.db.Exec(`INSERT INTO entities(kind, key, title, attrs) VALUES(?,?,?,?)
		ON CONFLICT(kind, key) DO UPDATE SET
		  title = CASE WHEN excluded.title = '' THEN entities.title ELSE excluded.title END,
		  attrs = CASE WHEN excluded.attrs = '{}' THEN entities.attrs ELSE excluded.attrs END`,
		kind, key, title, attrs)
	if err != nil {
		return Entity{}, err
	}
	e, err := s.Get(kind, key)
	if err != nil {
		return Entity{}, err
	}
	return e, s.index("entity", e.ID, e.Kind+" "+e.Key+" "+e.Title)
}

// Get fetches an entity by (kind, key).
func (s *Store) Get(kind, key string) (Entity, error) {
	var e Entity
	err := s.db.QueryRow(`SELECT id, kind, key, title, attrs, url, status, summary, updated_at FROM entities WHERE kind=? AND key=?`, kind, key).
		Scan(&e.ID, &e.Kind, &e.Key, &e.Title, &e.Attrs, &e.URL, &e.Status, &e.Summary, &e.UpdatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		return Entity{}, fmt.Errorf("graph: no %s %q", kind, key)
	}
	return e, err
}

func (s *Store) byID(id int64) (Entity, error) {
	var e Entity
	err := s.db.QueryRow(`SELECT id, kind, key, title, attrs, url, status, summary, updated_at FROM entities WHERE id=?`, id).
		Scan(&e.ID, &e.Kind, &e.Key, &e.Title, &e.Attrs, &e.URL, &e.Status, &e.Summary, &e.UpdatedAt)
	return e, err
}

// Alias records a source's own id for an entity (a Slack user id for a
// person, a Linear uuid for a ticket).
func (s *Store) Alias(entityID int64, source, foreignID, url string) error {
	_, err := s.db.Exec(`INSERT OR REPLACE INTO aliases(entity_id, source, foreign_id, url) VALUES(?,?,?,?)`,
		entityID, source, foreignID, nullable(url))
	return err
}

// AliasOwner finds the entity a source id was aliased to.
func (s *Store) AliasOwner(source, foreignID string) (int64, error) {
	var id int64
	err := s.db.QueryRow(`SELECT entity_id FROM aliases WHERE source=? AND foreign_id=?`, source, foreignID).Scan(&id)
	return id, err
}

// Link is the link truth of an entity: where it lives, what state it
// is in, one line about it, and the source's own timestamp. Empty
// fields keep what is stored, so a collector that knows only the url
// does not blank a summary another one wrote.
type Link struct {
	URL       string
	Status    string
	Summary   string
	UpdatedAt int64
}

// SetLink records an entity's link truth and mirrors the summary into
// FTS so search finds it by what it is about, not only by key.
func (s *Store) SetLink(e Entity, l Link) (Entity, error) {
	_, err := s.db.Exec(`UPDATE entities SET
		  url = CASE WHEN ? = '' THEN url ELSE ? END,
		  status = CASE WHEN ? = '' THEN status ELSE ? END,
		  summary = CASE WHEN ? = '' THEN summary ELSE ? END,
		  updated_at = CASE WHEN ? = 0 THEN updated_at ELSE ? END
		WHERE id = ?`,
		l.URL, l.URL, l.Status, l.Status, l.Summary, l.Summary, l.UpdatedAt, l.UpdatedAt, e.ID)
	if err != nil {
		return Entity{}, err
	}
	e, err = s.byID(e.ID)
	if err != nil {
		return Entity{}, err
	}
	return e, s.index("entity", e.ID, e.Kind+" "+e.Key+" "+e.Title+" "+e.Summary)
}

// SetState records e -has_state-> state:<status>, closing the previous
// state's window when it differs, so Timeline shows every transition
// and the status column shows the current one. at is the source's
// clock for the change (0: now).
func (s *Store) SetState(e Entity, status string, episode int64, author string, at int64) error {
	status = strings.ToLower(strings.TrimSpace(status))
	if status == "" {
		return nil
	}
	if at == 0 {
		at = s.unix()
	}
	open, err := s.Neighbors(e, 1, "has_state", 0)
	if err != nil {
		return err
	}
	for _, o := range open {
		if o.Src.ID != e.ID {
			continue
		}
		if o.Dst.Key == status {
			return nil // unchanged
		}
		if _, err := s.db.Exec(`UPDATE edges SET valid_to=? WHERE id=? AND valid_to IS NULL`, at, o.ID); err != nil {
			return err
		}
	}
	st, err := s.Upsert("state", status, status, "")
	if err != nil {
		return err
	}
	if _, err := s.Assert(e, "has_state", st, episode, author, AssertOpts{ValidFrom: at, ObservedAt: at}); err != nil {
		return err
	}
	_, err = s.SetLink(e, Link{Status: status})
	return err
}

// AssertOpts are the optional fields of a claim.
type AssertOpts struct {
	ValidFrom  int64 // 0: now
	ObservedAt int64 // 0: now
	Weight     float64
	Claim      string // free text of the claim, what search matches on
}

// Assert records src -rel-> dst as of now, citing episode, signed by
// author. An identical open edge (same src, rel, dst) is returned as is:
// asserting a fact twice is not two facts.
func (s *Store) Assert(src Entity, rel string, dst Entity, episode int64, author string, o AssertOpts) (Edge, error) {
	e, _, err := s.AssertNew(src, rel, dst, episode, author, o)
	return e, err
}

// AssertNew is Assert that also says whether it wrote a row (false: the
// open edge already existed), which is what a collector counts.
func (s *Store) AssertNew(src Entity, rel string, dst Entity, episode int64, author string, o AssertOpts) (Edge, bool, error) {
	if rel == "" || author == "" || episode == 0 {
		return Edge{}, false, errors.New("graph: an edge needs rel, author and an episode")
	}
	if err := checkRel(rel); err != nil {
		return Edge{}, false, err
	}
	var id int64
	err := s.db.QueryRow(`SELECT id FROM edges WHERE src=? AND rel=? AND dst=? AND valid_to IS NULL`, src.ID, rel, dst.ID).Scan(&id)
	if err == nil {
		e, err := s.edge(id)
		return e, false, err
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return Edge{}, false, err
	}
	now := s.unix()
	if o.ValidFrom == 0 {
		o.ValidFrom = now
	}
	if o.ObservedAt == 0 {
		o.ObservedAt = now
	}
	if o.Weight == 0 {
		o.Weight = 1
	}
	res, err := s.db.Exec(`INSERT INTO edges(src, rel, dst, valid_from, valid_to, observed_at, recorded_at, episode_id, author, weight, claim)
		VALUES(?,?,?,?,NULL,?,?,?,?,?,?)`,
		src.ID, rel, dst.ID, o.ValidFrom, o.ObservedAt, now, episode, author, o.Weight, o.Claim)
	if err != nil {
		return Edge{}, false, err
	}
	id, _ = res.LastInsertId()
	text := o.Claim
	if text == "" {
		text = src.Title + " " + rel + " " + dst.Title
	}
	if err := s.index("edge", id, text); err != nil {
		return Edge{}, false, err
	}
	e, err := s.edge(id)
	return e, true, err
}

// Invalidate closes an edge's validity window as of now (or at, when
// given), citing why in a new episode. The row stays; it is history.
func (s *Store) Invalidate(edgeID int64, reason, author string, at int64) error {
	if at == 0 {
		at = s.unix()
	}
	res, err := s.db.Exec(`UPDATE edges SET valid_to=? WHERE id=? AND valid_to IS NULL`, at, edgeID)
	if err != nil {
		return err
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return fmt.Errorf("graph: edge %d is not open", edgeID)
	}
	// The why is provenance too: an episode naming the edge and the reason.
	_, err = s.Episode("invalidate:"+author, fmt.Sprintf("edge:%d %s", edgeID, reason))
	return err
}

func (s *Store) edge(id int64) (Edge, error) {
	rows, err := s.db.Query(edgeSelect+` WHERE e.id=?`, id)
	if err != nil {
		return Edge{}, err
	}
	defer rows.Close()
	es, err := scanEdges(rows)
	if err != nil {
		return Edge{}, err
	}
	if len(es) == 0 {
		return Edge{}, fmt.Errorf("graph: no edge %d", id)
	}
	return es[0], nil
}

const edgeSelect = `SELECT e.id, e.rel, e.valid_from, e.valid_to, e.observed_at, e.recorded_at, e.episode_id, e.author, e.weight, e.claim,
  a.id, a.kind, a.key, a.title, a.attrs, a.url, a.status, a.summary, a.updated_at,
  b.id, b.kind, b.key, b.title, b.attrs, b.url, b.status, b.summary, b.updated_at
  FROM edges e JOIN entities a ON a.id=e.src JOIN entities b ON b.id=e.dst`

func scanEdges(rows *sql.Rows) ([]Edge, error) {
	var out []Edge
	for rows.Next() {
		var e Edge
		var vt sql.NullInt64
		if err := rows.Scan(&e.ID, &e.Rel, &e.ValidFrom, &vt, &e.ObservedAt, &e.RecordedAt, &e.EpisodeID, &e.Author, &e.Weight, &e.Claim,
			&e.Src.ID, &e.Src.Kind, &e.Src.Key, &e.Src.Title, &e.Src.Attrs, &e.Src.URL, &e.Src.Status, &e.Src.Summary, &e.Src.UpdatedAt,
			&e.Dst.ID, &e.Dst.Kind, &e.Dst.Key, &e.Dst.Title, &e.Dst.Attrs, &e.Dst.URL, &e.Dst.Status, &e.Dst.Summary, &e.Dst.UpdatedAt); err != nil {
			return nil, err
		}
		if vt.Valid {
			v := vt.Int64
			e.ValidTo = &v
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

// Neighbors expands hops from an entity over OPEN edges (both
// directions), optionally only one relation, and returns the edges
// crossed. at = 0 means now; otherwise edges valid at that instant.
func (s *Store) Neighbors(e Entity, hops int, rel string, at int64) ([]Edge, error) {
	if hops < 1 {
		hops = 1
	}
	if hops > 3 {
		hops = 3
	}
	if at == 0 {
		at = s.unix()
	}
	relClause := ""
	args := []any{e.ID, at, at}
	if rel != "" {
		relClause = " AND e.rel = ?"
		args = append(args, rel)
	}
	args = append(args, hops)
	// reach: every node within hops, at its shortest depth. An edge is
	// crossed when one of its ends sits STRICTLY inside the radius
	// (depth < hops); edges hanging off the frontier are the next hop.
	q := `WITH RECURSIVE reach(id, depth) AS (
	  SELECT ?, 0
	  UNION
	  SELECT CASE WHEN e.src = r.id THEN e.dst ELSE e.src END, r.depth + 1
	    FROM edges e JOIN reach r ON (e.src = r.id OR e.dst = r.id)
	    WHERE e.valid_from <= ? AND (e.valid_to IS NULL OR e.valid_to > ?)` + relClause + `
	      AND r.depth < ?
	), inner_nodes AS (
	  SELECT id FROM reach GROUP BY id HAVING MIN(depth) < ?
	)
	` + edgeSelect + `
	WHERE (e.src IN (SELECT id FROM inner_nodes) OR e.dst IN (SELECT id FROM inner_nodes))
	  AND e.valid_from <= ? AND (e.valid_to IS NULL OR e.valid_to > ?)` + relClause + `
	ORDER BY e.weight DESC, e.observed_at DESC`
	args = append(args, hops, at, at)
	if rel != "" {
		args = append(args, rel)
	}
	rows, err := s.db.Query(q, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanEdges(rows)
}

// Timeline is every edge touching an entity, closed windows included,
// newest observation first: "what changed".
func (s *Store) Timeline(e Entity) ([]Edge, error) {
	rows, err := s.db.Query(edgeSelect+` WHERE e.src=? OR e.dst=? ORDER BY e.observed_at DESC, e.id DESC`, e.ID, e.ID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanEdges(rows)
}

// Hit is one search result: an entity or an edge, with its fused score.
type Hit struct {
	Score  float64 `json:"score"`
	Entity *Entity `json:"entity,omitempty"`
	Edge   *Edge   `json:"edge,omitempty"`
}

// Search is FTS5 (BM25) and, with an embedder, cosine over the same
// text, merged by reciprocal-rank fusion (k = 60), then the top hits.
// Never calls a model; the one embedding call is for the query vector.
func (s *Store) Search(ctx context.Context, query string, limit int) ([]Hit, error) {
	if limit <= 0 {
		limit = 10
	}
	query = strings.TrimSpace(query)
	if query == "" {
		return nil, nil
	}
	type ref struct {
		kind string
		id   int64
	}
	ranks := map[ref]float64{}
	add := func(list []ref) {
		for i, r := range list {
			ranks[r] += 1.0 / float64(60+i+1)
		}
	}
	// Lexical.
	rows, err := s.db.Query(`SELECT kind, ref FROM graph_fts WHERE graph_fts MATCH ? ORDER BY bm25(graph_fts) LIMIT ?`, ftsQuery(query), limit*3)
	if err != nil {
		return nil, err
	}
	var lex []ref
	for rows.Next() {
		var r ref
		if err := rows.Scan(&r.kind, &r.id); err != nil {
			rows.Close()
			return nil, err
		}
		lex = append(lex, r)
	}
	rows.Close()
	add(lex)
	// Semantic.
	if s.embed != nil {
		if qv, err := s.embed.Embed(ctx, query); err == nil && len(qv) > 0 {
			vrows, err := s.db.Query(`SELECT kind, ref, vec FROM embeddings WHERE model=?`, s.embed.Model())
			if err != nil {
				return nil, err
			}
			type scored struct {
				r ref
				c float64
			}
			var all []scored
			for vrows.Next() {
				var r ref
				var blob []byte
				if err := vrows.Scan(&r.kind, &r.id, &blob); err != nil {
					vrows.Close()
					return nil, err
				}
				all = append(all, scored{r, cosine(qv, decode(blob))})
			}
			vrows.Close()
			slices.SortFunc(all, func(a, b scored) int { return cmp.Compare(b.c, a.c) })
			var sem []ref
			for i, a := range all {
				if i >= limit*3 || a.c < 0.2 {
					break
				}
				sem = append(sem, a.r)
			}
			add(sem)
		}
	}
	type kv struct {
		r ref
		s float64
	}
	var fused []kv
	for r, sc := range ranks {
		fused = append(fused, kv{r, sc})
	}
	slices.SortFunc(fused, func(a, b kv) int {
		return cmp.Or(cmp.Compare(b.s, a.s), cmp.Compare(a.r.id, b.r.id))
	})
	var out []Hit
	for _, f := range fused {
		if len(out) >= limit {
			break
		}
		switch f.r.kind {
		case "entity":
			e, err := s.byID(f.r.id)
			if err == nil {
				out = append(out, Hit{Score: f.s, Entity: &e})
			}
		case "edge":
			e, err := s.edge(f.r.id)
			if err == nil {
				out = append(out, Hit{Score: f.s, Edge: &e})
			}
		}
	}
	return out, nil
}

// ftsQuery quotes each term so punctuation in keys (NME-1673, repo#50)
// is matched literally instead of parsed as FTS syntax.
func ftsQuery(q string) string {
	var parts []string
	for f := range strings.FieldsSeq(q) {
		parts = append(parts, `"`+strings.ReplaceAll(f, `"`, `""`)+`"`)
	}
	return strings.Join(parts, " OR ")
}

// index mirrors text into FTS (replacing the row) and embeds it.
func (s *Store) index(kind string, id int64, text string) error {
	if _, err := s.db.Exec(`DELETE FROM graph_fts WHERE kind=? AND ref=?`, kind, id); err != nil {
		return err
	}
	if _, err := s.db.Exec(`INSERT INTO graph_fts(kind, ref, text) VALUES(?,?,?)`, kind, id, text); err != nil {
		return err
	}
	if s.embed == nil {
		return nil
	}
	v, err := s.embed.Embed(context.Background(), text)
	if err != nil || len(v) == 0 {
		return nil // embeddings are an accelerator; a failed call is not a failed write
	}
	_, err = s.db.Exec(`INSERT OR REPLACE INTO embeddings(kind, ref, model, vec) VALUES(?,?,?,?)`, kind, id, s.embed.Model(), encode(v))
	return err
}

// Stats is a count of everything, for `bough graph stats` and tests.
type Stats struct {
	Entities, Edges, OpenEdges, Episodes, Embeddings int
}

func (s *Store) Stats() (Stats, error) {
	var st Stats
	for _, q := range []struct {
		sql string
		dst *int
	}{
		{`SELECT count(*) FROM entities`, &st.Entities},
		{`SELECT count(*) FROM edges`, &st.Edges},
		{`SELECT count(*) FROM edges WHERE valid_to IS NULL`, &st.OpenEdges},
		{`SELECT count(*) FROM episodes`, &st.Episodes},
		{`SELECT count(*) FROM embeddings`, &st.Embeddings},
	} {
		if err := s.db.QueryRow(q.sql).Scan(q.dst); err != nil {
			return st, err
		}
	}
	return st, nil
}

// Entities lists entities of a kind (all kinds when kind == ""), by key.
func (s *Store) Entities(kind string) ([]Entity, error) {
	q, args := `SELECT id, kind, key, title, attrs, url, status, summary, updated_at FROM entities ORDER BY kind, key`, []any{}
	if kind != "" {
		q, args = `SELECT id, kind, key, title, attrs, url, status, summary, updated_at FROM entities WHERE kind=? ORDER BY key`, []any{kind}
	}
	rows, err := s.db.Query(q, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []Entity
	for rows.Next() {
		var e Entity
		if err := rows.Scan(&e.ID, &e.Kind, &e.Key, &e.Title, &e.Attrs, &e.URL, &e.Status, &e.Summary, &e.UpdatedAt); err != nil {
			return nil, err
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

func nullable(s string) any {
	if s == "" {
		return nil
	}
	return s
}

// encode/decode pack float32 vectors little-endian.
func encode(v []float32) []byte {
	b := make([]byte, 4*len(v))
	for i, f := range v {
		binary.LittleEndian.PutUint32(b[4*i:], math.Float32bits(f))
	}
	return b
}

func decode(b []byte) []float32 {
	v := make([]float32, len(b)/4)
	for i := range v {
		v[i] = math.Float32frombits(binary.LittleEndian.Uint32(b[4*i:]))
	}
	return v
}

func cosine(a, b []float32) float64 {
	if len(a) != len(b) || len(a) == 0 {
		return 0
	}
	var dot, na, nb float64
	for i := range a {
		dot += float64(a[i]) * float64(b[i])
		na += float64(a[i]) * float64(a[i])
		nb += float64(b[i]) * float64(b[i])
	}
	if na == 0 || nb == 0 {
		return 0
	}
	return dot / (math.Sqrt(na) * math.Sqrt(nb))
}
