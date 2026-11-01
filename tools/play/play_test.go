package main

import (
	"reflect"
	"strings"
	"testing"
	"time"
)

func TestReleasesSortNewestFirstNumerically(t *testing.T) {
	names := []string{"a0.9.0", "a0.10.0", "a0.1.0", "a1.0.0", "a0.10.2"}
	sortReleases(names)
	want := []string{"a1.0.0", "a0.10.2", "a0.10.0", "a0.9.0", "a0.1.0"}
	if !reflect.DeepEqual(names, want) {
		t.Fatalf("got %v, want %v", names, want)
	}
}

func TestParseVersionRejectsOtherTags(t *testing.T) {
	for _, tag := range []string{"v0.1.0", "a0.1", "a0.1.0-rc1", "alpha", "a1.2.x"} {
		if _, ok := parseVersion(tag); ok {
			t.Errorf("%q should not be a release", tag)
		}
	}
}

func TestFlakeRefEncodesThePath(t *testing.T) {
	got := flakeRef("/home/me/40-06_$deepdelve", "a0.1.0")
	want := "git+file:///home/me/40-06_%24deepdelve?ref=refs/tags/a0.1.0#default"
	if got != want {
		t.Fatalf("got %s, want %s", got, want)
	}
}

func TestRunStepsBuildOnlyWhenNeeded(t *testing.T) {
	steps := runSteps(t.TempDir(), "a0.1.0", []string{"--seed", "1"})
	if len(steps) != 2 || steps[0].argv[1] != "build" {
		t.Fatalf("an unbuilt release should build, then run: %v", steps)
	}
	last := steps[1].argv
	if !strings.HasSuffix(last[0], "/bin/deepdelve") || last[1] != "--seed" {
		t.Fatalf("wrong launch command: %v", last)
	}
}

func TestSplitLinesTreatsCarriageReturnAsLineEnd(t *testing.T) {
	data := []byte("10%\r20%\nend")
	var got []string
	for len(data) > 0 {
		n, tok, _ := splitLines(data, true)
		got = append(got, string(tok))
		data = data[n:]
	}
	if want := []string{"10%", "20%", "end"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("got %q, want %q", got, want)
	}
}

func collect(j *job) []string {
	var lines []string
	for line := range j.lines {
		lines = append(lines, line)
	}
	return lines
}

func TestJobStreamsOutputAndStopsOnFailure(t *testing.T) {
	j := startJob(t.TempDir(), []step{
		{label: "first", argv: []string{"sh", "-c", "echo out; echo err >&2"}},
		{label: "fails", argv: []string{"sh", "-c", "exit 3"}},
		{label: "never", argv: []string{"sh", "-c", "echo unreachable"}},
	})
	got := strings.Join(collect(j), "|")
	if !strings.Contains(got, "out") || !strings.Contains(got, "err") {
		t.Errorf("output missing: %s", got)
	}
	if strings.Contains(got, "unreachable") || j.err == nil {
		t.Errorf("should stop at the failure: %s, err %v", got, j.err)
	}
}

func TestStoppingAJobEndsItsChildren(t *testing.T) {
	j := startJob(t.TempDir(), []step{
		{label: "slow", argv: []string{"sh", "-c", "sleep 60 & wait"}},
		{label: "never", argv: []string{"sh", "-c", "echo unreachable"}},
	})
	time.Sleep(200 * time.Millisecond)
	started := time.Now()
	j.stop()
	lines := collect(j)
	if time.Since(started) > 10*time.Second {
		t.Fatalf("took too long to stop")
	}
	if j.err != errStopped || strings.Contains(strings.Join(lines, "|"), "unreachable") {
		t.Fatalf("err %v, lines %v", j.err, lines)
	}
}

func TestCleanReportDropsToolingLines(t *testing.T) {
	report := "$ deepdelve --bench 20\n\n==> nix develop . --command x\nwarning: Git tree is dirty\nseed 777\n  frame ms: mean 7\n"
	want := "$ deepdelve --bench 20\nseed 777\n  frame ms: mean 7\n"
	if got := cleanReport(report); got != want {
		t.Fatalf("got %q", got)
	}
}

func TestBenchSummaryKeepsComparableLines(t *testing.T) {
	report := "seed 777\nworld ready in 2s\n  worst frame was number 4\nbench: 10 frames\n  frame ms: mean 7\n  gpu ms: cull 1\n  peak queue: 0\n"
	want := "world ready in 2s\nbench: 10 frames\nframe ms: mean 7\ngpu ms: cull 1"
	if got := benchSummary(report); got != want {
		t.Fatalf("got %q", got)
	}
}
