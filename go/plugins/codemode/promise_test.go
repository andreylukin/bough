package codemode

import (
	"strings"
	"testing"
	"time"
)

// An async call's rejection is an error even when its Promise is not the
// block's last value; a handled rejection is not.
func TestUnhandledRejectionMidBlock(t *testing.T) {
	cm := New(2 * time.Second)
	for _, code := range []string{
		`(async()=>{throw new Error("MID_BOOM")})(); "after"`,
		`const r = (async()=>{throw new Error("MID_BOOM")})(); console.log("x")`,
	} {
		out, err := cm.Run(code)
		if err == nil || !strings.Contains(err.Error(), "MID_BOOM") {
			t.Errorf("Run(%s) = (%q, %v): rejection swallowed", code, out, err)
		}
	}
	if out, err := cm.Run(`(async()=>{throw new Error("H")})().catch(()=>{}); "ok"`); err != nil || out != "ok" {
		t.Errorf("handled rejection = (%q, %v)", out, err)
	}
	if out, err := cm.Run(`"alive"`); err != nil || out != "alive" {
		t.Errorf("next block = (%q, %v)", out, err)
	}
}
