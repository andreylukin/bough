package main

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"text/tabwriter"
	"time"

	"github.com/andreylukin/bough/internal/pipeline"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
)

const loopUsage = `usage:
  bough loop run <pipeline.yml> [--detach] [--set id.key=value ...]
  bough loop status [<run-id>]
  bough loop stop <run-id>`

// runLoop is `bough loop`: deterministic pipelines (docs/loops.md).
func runLoop(args []string) {
	if len(args) == 0 {
		fmt.Fprintln(os.Stderr, loopUsage)
		os.Exit(2)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		os.Exit(1)
	}
	switch args[0] {
	case "run":
		os.Exit(loopRun(home, args[1:]))
	case "status":
		os.Exit(loopStatus(home, args[1:]))
	case "stop":
		os.Exit(loopStop(home, args[1:]))
	default:
		fmt.Fprintln(os.Stderr, loopUsage)
		os.Exit(2)
	}
}

// supHolder is the runner's Sessions: the supervisor keeps its meta in
// the run dir, which exists only once NewRunner has minted the id.
type supHolder struct{ *serve.Supervisor }

func loopRun(home string, args []string) int {
	var path string
	var sets []string
	detached := false
	for i := 0; i < len(args); i++ {
		a := args[i]
		switch {
		case a == "--detach" || a == "-detach":
			detached = true
		case a == "--set" || a == "-set":
			if i+1 >= len(args) {
				fmt.Fprintln(os.Stderr, "bough loop run: --set needs a value")
				return 2
			}
			i++
			sets = append(sets, args[i])
		case strings.HasPrefix(a, "--set="):
			sets = append(sets, strings.TrimPrefix(a, "--set="))
		case path == "" && !strings.HasPrefix(a, "-"):
			path = a
		default:
			fmt.Fprintf(os.Stderr, "bough loop run: unexpected %q\n%s\n", a, loopUsage)
			return 2
		}
	}
	if path == "" {
		fmt.Fprintln(os.Stderr, loopUsage)
		return 2
	}
	p, err := pipeline.Load(path)
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		return 2
	}
	// A detached runner is handed its id so the parent can name the run.
	id := os.Getenv("BOUGH_LOOP_ID")
	os.Unsetenv("BOUGH_LOOP_ID")
	if detached {
		return loopDetach(home, args)
	}

	holder := &supHolder{}
	r, err := pipeline.NewRunner(p, pipeline.Options{Home: home, Sessions: holder, Sets: sets, Out: os.Stdout, ID: id})
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		return 2
	}
	exe, _ := os.Executable()
	sup, err := serve.NewSupervisor(serve.Options{Exe: exe, HistDir: sessionsDir(), MetaPath: filepath.Join(r.Dir(), "meta.json"), Home: home})
	if err != nil {
		r.Fail(err.Error())
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		return 1
	}
	holder.Supervisor = sup
	defer sup.Close()
	fmt.Printf("run %s (%s)\n", r.ID(), r.Dir())

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	st, err := r.Run(ctx)
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
	}
	switch st.Status {
	case pipeline.StatusPassed:
		return 0
	case pipeline.StatusExhausted:
		return 3
	case pipeline.StatusStopped:
		return 130
	default:
		return 1
	}
}

// loopDetach re-execs the run in its own session with output in
// <runDir>/runner.log, and prints the run id.
func loopDetach(home string, args []string) int {
	id := history.NewID()
	dir := filepath.Join(pipeline.RunsDir(home), id)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		return 1
	}
	log, err := os.Create(filepath.Join(dir, "runner.log"))
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		return 1
	}
	defer log.Close()
	rest := []string{"loop", "run"}
	for _, a := range args {
		if a != "--detach" && a != "-detach" {
			rest = append(rest, a)
		}
	}
	exe, _ := os.Executable()
	c := exec.Command(exe, rest...)
	c.Env = append(os.Environ(), "BOUGH_LOOP_ID="+id)
	c.Stdout, c.Stderr = log, log
	detach(c)
	if err := c.Start(); err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		return 1
	}
	fmt.Println(id)
	fmt.Fprintln(os.Stderr, "log:", log.Name())
	return 0
}

func runStatus(s pipeline.State) string {
	if s.Status == pipeline.StatusRunning && !alive(s.Pid) {
		return "running (dead)"
	}
	return s.Status
}

func loopStatus(home string, args []string) int {
	if len(args) == 0 {
		runs, err := pipeline.ListRuns(home)
		if err != nil {
			fmt.Fprintln(os.Stderr, "bough loop:", err)
			return 1
		}
		if len(runs) == 0 {
			fmt.Println("no loop runs")
			return 0
		}
		w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
		fmt.Fprintln(w, "ID\tNAME\tSTATUS\tNODE\tSTEP\tAGE")
		for _, s := range runs {
			fmt.Fprintf(w, "%s\t%s\t%s\t%s\t%d\t%s\n", s.ID, s.Name, runStatus(s), s.Node, s.Step, time.Since(s.Started).Round(time.Second))
		}
		w.Flush()
		return 0
	}
	s, err := pipeline.ReadState(home, args[0])
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		return 1
	}
	fmt.Printf("id:       %s\nname:     %s\nstatus:   %s\nnode:     %s\nstep:     %d\nvisits:   %v\nsessions: %v\nstarted:  %s\n",
		s.ID, s.Name, runStatus(s), s.Node, s.Step, s.Visits, s.Sessions, s.Started.Format(time.RFC3339))
	if s.Reason != "" {
		fmt.Printf("reason:   %s\n", s.Reason)
	}
	if !s.Ended.IsZero() {
		fmt.Printf("ended:    %s\n", s.Ended.Format(time.RFC3339))
	}
	evs, _ := pipeline.ReadEvents(home, args[0])
	if len(evs) > 10 {
		evs = evs[len(evs)-10:]
	}
	fmt.Println("events:")
	for _, e := range evs {
		text := strings.ReplaceAll(e.Text, "\n", " ")
		if len(text) > 80 {
			text = text[:80] + "…"
		}
		fmt.Printf("  %s %-6s %d %s %s\n", e.At.Format("15:04:05"), e.Kind, e.Step, e.Node, text)
	}
	return 0
}

func loopStop(home string, args []string) int {
	if len(args) != 1 {
		fmt.Fprintln(os.Stderr, loopUsage)
		return 2
	}
	s, err := pipeline.ReadState(home, args[0])
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough loop:", err)
		return 1
	}
	if s.Status != pipeline.StatusRunning || !alive(s.Pid) {
		fmt.Printf("%s: %s\n", s.ID, runStatus(s))
		return 0
	}
	proc, err := os.FindProcess(s.Pid)
	if err == nil {
		err = proc.Signal(os.Interrupt)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough loop stop:", err)
		return 1
	}
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(100 * time.Millisecond) {
		if s, err = pipeline.ReadState(home, args[0]); err == nil && s.Status != pipeline.StatusRunning {
			fmt.Printf("%s: %s\n", s.ID, s.Status)
			return 0
		}
	}
	fmt.Fprintf(os.Stderr, "bough loop stop: %s still running after 10s\n", args[0])
	return 1
}
