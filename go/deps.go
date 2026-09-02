//go:build deps

// Package bough pins plugin dependencies in go.mod before the plugin
// packages that import them exist. Never built; delete once all
// plugins land. Plugin agents: do NOT run go get — deps are pinned.
package bough

import (
	_ "charm.land/bubbles/v2/textinput"
	_ "charm.land/bubbletea/v2"
	_ "charm.land/lipgloss/v2"
	_ "github.com/Gaurav-Gosain/sip"
	_ "github.com/anthropics/anthropic-sdk-go"
	_ "github.com/charmbracelet/bubbles/textinput"
	_ "github.com/charmbracelet/bubbles/viewport"
	_ "github.com/charmbracelet/bubbletea"
	_ "github.com/charmbracelet/lipgloss"
	_ "github.com/dop251/goja"
	_ "gopkg.in/yaml.v3"
)
