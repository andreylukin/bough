// Package tracecheck replays an abstract trace of (action, state) steps
// against a FizzBee state graph and reports the first step the model does
// not allow. The graph is the protobuf pair fizz writes into its run dir
// (go/tests/model/GRAPH.md); it is decoded from the wire format by hand so
// the test tree needs no generated code and no new module dependency.
package tracecheck

import (
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
)

// Node is one state of the graph. Index is its position across all
// nodes_*.pb shards; node 0 is the state Init produced.
type Node struct {
	Index int
	// Name is the yield point: "yield" for a settled state, the action
	// name for a state paused inside a non-atomic step or a fork.
	Name  string
	State map[string]any
}

// Link is one edge. Name is the spec's action name ("Role.Action" for
// roles); a fork inside an action shows up as further links out of an
// intermediate node, named after the choice (e.g. "Any:locked=True").
type Link struct {
	Src, Dest int
	Name      string
	Type      string
}

// Graph is a decoded fizz run dir.
type Graph struct {
	Nodes []Node
	Links []Link
	out   [][]int // link indices by source node
}

// Load reads every nodes_*.pb and adjacency_lists_*.pb shard in dir. Shards
// are read in file-name order because node indices are global across them.
func Load(dir string) (*Graph, error) {
	nodeFiles, _ := filepath.Glob(filepath.Join(dir, "nodes_*_of_*.pb"))
	linkFiles, _ := filepath.Glob(filepath.Join(dir, "adjacency_lists_*_of_*.pb"))
	if len(nodeFiles) == 0 || len(linkFiles) == 0 {
		return nil, fmt.Errorf("tracecheck: %s has no nodes_*/adjacency_lists_* shards (a failing fizz run writes only the error trace)", dir)
	}
	sort.Strings(nodeFiles)
	sort.Strings(linkFiles)
	g := &Graph{}
	for _, f := range nodeFiles {
		b, err := os.ReadFile(f)
		if err != nil {
			return nil, fmt.Errorf("tracecheck: %w", err)
		}
		if err := g.decodeNodes(b); err != nil {
			return nil, fmt.Errorf("tracecheck: %s: %w", f, err)
		}
	}
	for _, f := range linkFiles {
		b, err := os.ReadFile(f)
		if err != nil {
			return nil, fmt.Errorf("tracecheck: %w", err)
		}
		if err := g.decodeLinks(b); err != nil {
			return nil, fmt.Errorf("tracecheck: %s: %w", f, err)
		}
	}
	if err := g.index(); err != nil {
		return nil, err
	}
	return g, nil
}

func (g *Graph) index() error {
	g.out = make([][]int, len(g.Nodes))
	for i, l := range g.Links {
		if l.Src < 0 || l.Src >= len(g.Nodes) || l.Dest < 0 || l.Dest >= len(g.Nodes) {
			return fmt.Errorf("tracecheck: link %d (%s) points outside %d nodes", i, l.Name, len(g.Nodes))
		}
		g.out[l.Src] = append(g.out[l.Src], i)
	}
	return nil
}

// field walks one protobuf message, calling fn for every field. Only the
// wire types graph.proto uses are accepted; anything else is corruption.
func fields(b []byte, fn func(num int, typ int, v uint64, bytes []byte) error) error {
	for len(b) > 0 {
		tag, n := binary.Uvarint(b)
		if n <= 0 {
			return errors.New("bad tag varint")
		}
		b = b[n:]
		num, typ := int(tag>>3), int(tag&7)
		switch typ {
		case 0:
			v, n := binary.Uvarint(b)
			if n <= 0 {
				return errors.New("bad varint")
			}
			b = b[n:]
			if err := fn(num, typ, v, nil); err != nil {
				return err
			}
		case 1:
			if len(b) < 8 {
				return errors.New("short fixed64")
			}
			v := binary.LittleEndian.Uint64(b)
			b = b[8:]
			if err := fn(num, typ, v, nil); err != nil {
				return err
			}
		case 2:
			l, n := binary.Uvarint(b)
			if n <= 0 || uint64(len(b)-n) < l {
				return errors.New("bad length-delimited field")
			}
			if err := fn(num, typ, 0, b[n:n+int(l)]); err != nil {
				return err
			}
			b = b[n+int(l):]
		case 5:
			if len(b) < 4 {
				return errors.New("short fixed32")
			}
			v := uint64(binary.LittleEndian.Uint32(b))
			b = b[4:]
			if err := fn(num, typ, v, nil); err != nil {
				return err
			}
		default:
			return fmt.Errorf("unsupported wire type %d", typ)
		}
	}
	return nil
}

// decodeNodes: message Nodes { repeated string json = 1; }
func (g *Graph) decodeNodes(b []byte) error {
	return fields(b, func(num, typ int, _ uint64, raw []byte) error {
		if num != 1 || typ != 2 {
			return nil
		}
		var n struct {
			Name  string         `json:"name"`
			State map[string]any `json:"state"`
			Roles []struct {
				Ref    string         `json:"ref_string"`
				Fields map[string]any `json:"fields"`
			} `json:"roles"`
		}
		if err := json.Unmarshal(raw, &n); err != nil {
			return fmt.Errorf("node %d: %w", len(g.Nodes), err)
		}
		// A global holding a role is only a reference ("role Session#0");
		// the role's fields are the state, qualified the way fizzbee-mbt
		// names them so two instances of a role never collide.
		if len(n.Roles) > 0 && n.State == nil {
			n.State = map[string]any{}
		}
		for _, r := range n.Roles {
			for k, v := range r.Fields {
				n.State[r.Ref+"."+k] = v
			}
		}
		g.Nodes = append(g.Nodes, Node{Index: len(g.Nodes), Name: n.Name, State: n.State})
		return nil
	})
}

// decodeLinks: message Links { int64 total_nodes = 1; repeated Link links = 2; }
// with Link { int64 src = 1; int64 dest = 2; string name = 3; ... string type = 9; }.
// proto3 omits zero values, so a link out of node 0 has no src field at all.
func (g *Graph) decodeLinks(b []byte) error {
	return fields(b, func(num, typ int, _ uint64, raw []byte) error {
		if num != 2 || typ != 2 {
			return nil
		}
		var l Link
		err := fields(raw, func(num, typ int, v uint64, s []byte) error {
			switch {
			case num == 1 && typ == 0:
				l.Src = varintInt(v)
			case num == 2 && typ == 0:
				l.Dest = varintInt(v)
			case num == 3 && typ == 2:
				l.Name = string(s)
			case num == 9 && typ == 2:
				l.Type = string(s)
			}
			return nil
		})
		if err != nil {
			return fmt.Errorf("link %d: %w", len(g.Links), err)
		}
		g.Links = append(g.Links, l)
		return nil
	})
}

func varintInt(v uint64) int {
	if v > math.MaxInt {
		return -1 // rejected by the bounds check in Load
	}
	return int(v)
}
