//go:build !windows

package engine

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/unreal/session"
	"github.com/andreylukin/bough/plugins/loop"
)

// settings is the row's config (§6.1) as the session reads it, plus
// what only the row itself needs.
type settings struct {
	session.Config
	Store string
	// keepWhole is set when a loop overlay's keep_whole_results reached
	// this row: accepted, ignored, and said once, because the engine's
	// context is append-only and nothing is trimmed.
	keepWhole bool
}

// readSettings parses the row config. Keys the engine does not know are
// ignored, as the loop ignores them: a home overlay written for the
// loop row must not stop the engine from mounting on the same row id.
func readSettings(cfg map[string]any) (settings, error) {
	s := settings{Config: session.Config{
		Tools:       "native",
		TurnSettle:  60 * time.Second,
		CallTimeout: 10 * time.Minute,
		MaxOutput:   40000,
		RowOutput:   8192,
		MaxSteps:    100,
		StopRetries: 2,
	}}
	for k, v := range cfg {
		var err error
		switch k {
		case "tools":
			str, _ := v.(string)
			if str != "native" && str != "both" {
				return s, fmt.Errorf("engine-unreal: tools must be native or both, got %v", v)
			}
			s.Tools = str
		case "turn_settle":
			s.TurnSettle, err = duration(k, v, false)
		case "heartbeat":
			s.Heartbeat, err = duration(k, v, true)
		case "call_timeout":
			s.CallTimeout, err = duration(k, v, false)
		case "max_output":
			s.MaxOutput, err = positive(k, v)
			if err == nil && s.MaxOutput > 1000000 {
				err = fmt.Errorf("engine-unreal: max_output must be at most 1000000, got %v", v)
			}
		case "row_output":
			s.RowOutput, err = positive(k, v)
		case "max_steps":
			s.MaxSteps, err = positive(k, v)
		case "stop_retries":
			n, ok := number(v)
			if !ok || n < 0 || n != float64(int(n)) {
				err = fmt.Errorf("engine-unreal: stop_retries must be a non-negative integer, got %v", v)
			}
			s.StopRetries = int(n)
		case "max_cost_usd":
			n, ok := number(v)
			if !ok || n < 0 {
				err = fmt.Errorf("engine-unreal: max_cost_usd must be a non-negative number, got %v", v)
			}
			s.MaxCostUSD = n
		case "steer_interrupts":
			s.SteerInterrupts, err = boolean(k, v)
		case "trace":
			s.Trace, err = boolean(k, v)
		case "system_prompt":
			str, ok := v.(string)
			if !ok || strings.TrimSpace(str) == "" {
				err = fmt.Errorf("engine-unreal: system_prompt must be a non-empty string")
			}
			s.SystemPrompt = str
		case "task_guidance":
			switch t := v.(type) {
			case bool:
				if t {
					s.TaskGuidance = loop.TaskGuidance
				}
			case string:
				switch strings.TrimSpace(t) {
				case "true":
					s.TaskGuidance = loop.TaskGuidance
				case "false", "":
				default:
					s.TaskGuidance = t
				}
			default:
				err = fmt.Errorf("engine-unreal: task_guidance must be a bool or the guidance text, got %v", v)
			}
		case "store":
			str, ok := v.(string)
			if !ok || strings.TrimSpace(str) == "" {
				err = fmt.Errorf("engine-unreal: store must be a directory path, got %v", v)
			}
			s.Store = expandHome(str)
		case "keep_whole_results":
			s.keepWhole = true
		}
		if err != nil {
			return s, err
		}
	}
	return s, nil
}

func duration(key string, v any, zero bool) (time.Duration, error) {
	var d time.Duration
	switch t := v.(type) {
	case string:
		var err error
		d, err = time.ParseDuration(strings.TrimSpace(t))
		if err != nil {
			return 0, fmt.Errorf("engine-unreal: %s must be a duration like 60s, got %v", key, v)
		}
	default:
		n, ok := number(v)
		if !ok {
			return 0, fmt.Errorf("engine-unreal: %s must be a duration like 60s, got %v", key, v)
		}
		d = time.Duration(n * float64(time.Second))
	}
	if d < 0 || d == 0 && !zero {
		return 0, fmt.Errorf("engine-unreal: %s must be a positive duration like 60s, got %v", key, v)
	}
	return d, nil
}

func positive(key string, v any) (int, error) {
	n, ok := number(v)
	if !ok || n < 1 || n != float64(int(n)) {
		return 0, fmt.Errorf("engine-unreal: %s must be a positive integer, got %v", key, v)
	}
	return int(n), nil
}

func boolean(key string, v any) (bool, error) {
	switch t := v.(type) {
	case bool:
		return t, nil
	case string:
		if b, err := strconv.ParseBool(t); err == nil {
			return b, nil
		}
	}
	return false, fmt.Errorf("engine-unreal: %s must be true or false, got %v", key, v)
}

func number(v any) (float64, bool) {
	switch n := v.(type) {
	case int:
		return float64(n), true
	case int64:
		return float64(n), true
	case float64:
		return n, true
	case string:
		f, err := strconv.ParseFloat(strings.TrimSpace(n), 64)
		return f, err == nil
	}
	return 0, false
}

func expandHome(p string) string {
	if p == "~" || strings.HasPrefix(p, "~/") {
		if home, err := os.UserHomeDir(); err == nil {
			return filepath.Join(home, strings.TrimPrefix(p, "~"))
		}
	}
	return p
}
