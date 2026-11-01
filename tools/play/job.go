package main

import (
	"bufio"
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"regexp"
	"sync/atomic"
	"syscall"
	"time"
)

var errStopped = errors.New("stopped")

var ansiEscape = regexp.MustCompile(`\x1b\[[0-9;?]*[ -/]*[@-~]`)

// job runs steps one after another in the background, streaming their output. Read
// lines until it's closed, and err is then final.
type job struct {
	lines   chan string
	err     error
	pgid    atomic.Int32
	stopped atomic.Bool
}

func startJob(root string, steps []step) *job {
	j := &job{lines: make(chan string, 1024)}
	go func() {
		defer close(j.lines)
		for _, s := range steps {
			j.lines <- "==> " + s.label
			if err := j.runStep(root, s); err != nil {
				j.err = err
				return
			}
			if j.stopped.Load() {
				j.err = errStopped
				return
			}
		}
	}()
	return j
}

func (j *job) runStep(root string, s step) error {
	r, w, err := os.Pipe()
	if err != nil {
		return err
	}
	defer r.Close()
	cmd := exec.Command(s.argv[0], s.argv[1:]...)
	cmd.Dir = root
	cmd.Stdout = w
	cmd.Stderr = w
	// Its own process group, so stopping also reaches cargo and the game under nix.
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		w.Close()
		return fmt.Errorf("%s: %w", s.argv[0], err)
	}
	w.Close()
	j.pgid.Store(int32(cmd.Process.Pid))

	scan := bufio.NewScanner(r)
	scan.Buffer(make([]byte, 64*1024), 4*1024*1024)
	scan.Split(splitLines)
	for scan.Scan() {
		if line := ansiEscape.ReplaceAllString(scan.Text(), ""); line != "" {
			j.lines <- line
		}
	}
	err = cmd.Wait()
	j.pgid.Store(0)
	if j.stopped.Load() {
		return errStopped
	}
	return err
}

// splitLines also ends a line at a carriage return, which progress displays use.
func splitLines(data []byte, atEOF bool) (int, []byte, error) {
	if i := bytes.IndexAny(data, "\r\n"); i >= 0 {
		return i + 1, data[:i], nil
	}
	if atEOF && len(data) > 0 {
		return len(data), data, nil
	}
	return 0, nil, nil
}

// stop asks the running step to end, then insists after a few seconds.
func (j *job) stop() {
	j.stopped.Store(true)
	pgid := int(j.pgid.Load())
	if pgid == 0 {
		return
	}
	_ = syscall.Kill(-pgid, syscall.SIGTERM)
	time.AfterFunc(3*time.Second, func() {
		if int(j.pgid.Load()) == pgid {
			_ = syscall.Kill(-pgid, syscall.SIGKILL)
		}
	})
}
