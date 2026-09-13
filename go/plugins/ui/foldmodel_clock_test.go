package ui

import "regexp"

// foldModelClock matches a running card's live elapsed chip; it ticks
// with the wall clock, so two frames a second apart differ there alone.
var foldModelClock = regexp.MustCompile(`· \d+s\b`)

func foldModelNoClock(s string) string { return foldModelClock.ReplaceAllString(stripANSI(s), "· Ns") }
