package artifacts

// Patches: a published page changes by a list of small operations on
// a path — "blocks[2].rows", "data.sales", "blocks" — instead of a
// full resend. The patched page goes through Normalize again, so a
// bad patch is refused with the same block-addressed message as a bad
// publish, and the page on disk is never left broken.

import (
	"errors"
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

var segRe = regexp.MustCompile(`^([A-Za-z_][A-Za-z0-9_-]*)((?:\[\d+\])*)$`)

// segment is one step of a path: a key, then zero or more indexes.
type segment struct {
	key   string
	index []int
}

func parsePath(path string) ([]segment, error) {
	if strings.TrimSpace(path) == "" {
		return nil, errors.New("path is empty")
	}
	var segs []segment
	for _, part := range strings.Split(path, ".") {
		m := segRe.FindStringSubmatch(part)
		if m == nil {
			return nil, fmt.Errorf("path %q: bad segment %q (want key, key[3], key.sub)", path, part)
		}
		seg := segment{key: m[1]}
		for _, ix := range regexp.MustCompile(`\d+`).FindAllString(m[2], -1) {
			n, _ := strconv.Atoi(ix)
			seg.index = append(seg.index, n)
		}
		segs = append(segs, seg)
	}
	return segs, nil
}

// Apply runs ops on page in order. Each op is {op: set|append|remove,
// path, value?}. set creates a missing key; append needs an array at
// path; remove deletes a key or an array element.
func Apply(page map[string]any, ops []any) error {
	if len(ops) == 0 {
		return errors.New("ops is empty")
	}
	for i, raw := range ops {
		op, ok := raw.(map[string]any)
		if !ok {
			return fmt.Errorf("ops[%d]: must be {op, path, value?}", i)
		}
		kind, _ := op["op"].(string)
		path, _ := op["path"].(string)
		segs, err := parsePath(path)
		if err != nil {
			return fmt.Errorf("ops[%d]: %w", i, err)
		}
		value, hasValue := op["value"]
		if err := applyOne(page, kind, segs, value, hasValue); err != nil {
			return fmt.Errorf("ops[%d] (%s %s): %w", i, kind, path, err)
		}
	}
	return nil
}

func applyOne(root map[string]any, kind string, segs []segment, value any, hasValue bool) error {
	// Walk to the container of the last step.
	var cur any = root
	for i, seg := range segs {
		last := i == len(segs)-1
		m, ok := cur.(map[string]any)
		if !ok {
			return fmt.Errorf("%s: not an object", seg.key)
		}
		if last && len(seg.index) == 0 {
			switch kind {
			case "set":
				if !hasValue {
					return errors.New("set needs a value")
				}
				m[seg.key] = value
			case "append":
				if !hasValue {
					return errors.New("append needs a value")
				}
				list, ok := m[seg.key].([]any)
				if !ok {
					return fmt.Errorf("%s is not an array", seg.key)
				}
				m[seg.key] = append(list, value)
			case "remove":
				if _, ok := m[seg.key]; !ok {
					return fmt.Errorf("%s: nothing there", seg.key)
				}
				delete(m, seg.key)
			default:
				return fmt.Errorf("op must be set, append or remove, got %q", kind)
			}
			return nil
		}
		next, ok := m[seg.key]
		if !ok {
			return fmt.Errorf("%s: nothing there", seg.key)
		}
		// Indexes: descend through arrays; the final index is the target.
		for j, ix := range seg.index {
			list, ok := next.([]any)
			if !ok {
				return fmt.Errorf("%s is not an array", seg.key)
			}
			if ix < 0 || ix >= len(list) {
				return fmt.Errorf("%s[%d]: out of range (length %d)", seg.key, ix, len(list))
			}
			if last && j == len(seg.index)-1 {
				switch kind {
				case "set":
					if !hasValue {
						return errors.New("set needs a value")
					}
					list[ix] = value
				case "append":
					inner, ok := list[ix].([]any)
					if !ok || !hasValue {
						return fmt.Errorf("%s[%d] is not an array", seg.key, ix)
					}
					list[ix] = append(inner, value)
				case "remove":
					m[seg.key] = append(list[:ix:ix], list[ix+1:]...)
					// Only valid when this is the outermost index.
					if j != 0 {
						return errors.New("remove on a nested index is not supported; set the parent instead")
					}
				default:
					return fmt.Errorf("op must be set, append or remove, got %q", kind)
				}
				return nil
			}
			next = list[ix]
		}
		cur = next
	}
	return nil
}
