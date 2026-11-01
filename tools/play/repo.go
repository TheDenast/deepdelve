package main

import (
	"encoding/json"
	"fmt"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
)

// A release is an annotated git tag named after versionPattern, plus its folder under
// releases/. Nothing about that depends on Nix: only buildSteps does, which is where
// recipes for other platforms would go.
var versionPattern = regexp.MustCompile(`^a(\d+)\.(\d+)\.(\d+)$`)

const hostTarget = "x86_64-linux"

const currentName = "current"

type version [3]int

func parseVersion(name string) (version, bool) {
	m := versionPattern.FindStringSubmatch(name)
	if m == nil {
		return version{}, false
	}
	var v version
	for i := range v {
		n, err := strconv.Atoi(m[i+1])
		if err != nil {
			return version{}, false
		}
		v[i] = n
	}
	return v, true
}

// sortReleases orders names newest first, numerically, so a0.10.0 follows a0.9.0.
func sortReleases(names []string) {
	sort.Slice(names, func(i, j int) bool {
		a, _ := parseVersion(names[i])
		b, _ := parseVersion(names[j])
		for k := range a {
			if a[k] != b[k] {
				return a[k] > b[k]
			}
		}
		return false
	})
}

func findRoot() (string, error) {
	out, err := exec.Command("git", "rev-parse", "--show-toplevel").Output()
	if err != nil {
		return "", fmt.Errorf("run this inside the deepdelve repository")
	}
	return strings.TrimSpace(string(out)), nil
}

// git returns a command's output without its final newline, or nothing if it failed.
// Leading space stays, since it's part of `git status --short`.
func git(root string, args ...string) string {
	cmd := exec.Command("git", args...)
	cmd.Dir = root
	out, err := cmd.Output()
	if err != nil {
		return ""
	}
	return strings.TrimRight(string(out), " \r\n")
}

func listReleases(root string) []string {
	var names []string
	for _, tag := range strings.Fields(git(root, "tag", "--list")) {
		if _, ok := parseVersion(tag); ok {
			names = append(names, tag)
		}
	}
	sortReleases(names)
	return names
}

func releaseDir(root, name string) string {
	return filepath.Join(root, "releases", name)
}

// buildLink is where nix build leaves its result. The link also protects the build and
// everything it needs from garbage collection.
func buildLink(root, name string) string {
	return filepath.Join(root, ".builds", name, hostTarget)
}

func gameBinary(root, name string) string {
	return filepath.Join(buildLink(root, name), "bin", "deepdelve")
}

func isBuilt(root, name string) bool {
	_, err := os.Stat(gameBinary(root, name))
	return err == nil
}

// flakeRef names the flake at a tag. Paths must be percent-encoded here, and this
// repository's own path contains a '$'.
func flakeRef(root, tag string) string {
	parts := strings.Split(root, "/")
	for i, p := range parts {
		parts[i] = url.PathEscape(p)
	}
	path := strings.ReplaceAll(strings.Join(parts, "/"), "$", "%24")
	return fmt.Sprintf("git+file://%s?ref=refs/tags/%s#default", path, tag)
}

// step is one command in a sequence of work.
type step struct {
	label string
	argv  []string
}

func devShell(command ...string) []string {
	return append([]string{"nix", "develop", ".", "--command"}, command...)
}

func buildSteps(root, name string) []step {
	return []step{{
		label: "Building " + name + " from its tag",
		argv: []string{
			"nix", "build", flakeRef(root, name),
			"--print-build-logs", "--out-link", buildLink(root, name),
		},
	}}
}

func runSteps(root, name string, args []string) []step {
	if name == currentName {
		command := append([]string{"cargo", "run", "--release", "--"}, args...)
		return []step{{label: "Building and running the working tree", argv: devShell(command...)}}
	}
	var steps []step
	if !isBuilt(root, name) {
		steps = buildSteps(root, name)
	}
	argv := append([]string{gameBinary(root, name)}, args...)
	return append(steps, step{label: "Running " + name, argv: argv})
}

// settings keeps each version's launch options between sessions.
type settings map[string]string

func settingsPath(root string) string {
	return filepath.Join(root, ".builds", "settings.json")
}

func loadSettings(root string) settings {
	s := settings{}
	if data, err := os.ReadFile(settingsPath(root)); err == nil {
		_ = json.Unmarshal(data, &s)
	}
	return s
}

func (s settings) save(root string) error {
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(settingsPath(root)), 0o755); err != nil {
		return err
	}
	return os.WriteFile(settingsPath(root), append(data, '\n'), 0o644)
}
