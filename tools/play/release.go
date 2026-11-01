package main

import (
	"bytes"
	"fmt"
	"image"
	"image/jpeg"
	_ "image/png"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"

	"golang.org/x/image/draw"
)

const (
	// The fixed world and viewpoint that makes snapshots comparable between versions.
	snapshotSeed = "777"
	snapshotView = "326,110,-1106,0.6,-0.12"
	snapshotSize = "2560x1440"

	screenshotWidth   = 1920
	screenshotQuality = 85
)

var cargoVersion = regexp.MustCompile(`(?m)^version = "([^"]+)"$`)

func runVisible(root string, out io.Writer, argv ...string) error {
	fmt.Fprintf(out, "\n==> %s\n", strings.Join(argv, " "))
	cmd := exec.Command(argv[0], argv[1:]...)
	cmd.Dir = root
	cmd.Stdin = os.Stdin
	cmd.Stdout = out
	cmd.Stderr = out
	return cmd.Run()
}

// checkRelease lists everything that stops name from being released right now.
func checkRelease(root, name string) []string {
	var problems []string
	if _, ok := parseVersion(name); !ok {
		return []string{fmt.Sprintf("%q is not a version name like a0.1.0", name)}
	}
	if git(root, "rev-parse", "--verify", "--quiet", "refs/tags/"+name) != "" {
		problems = append(problems, "tag "+name+" already exists")
	}
	if dirty := git(root, "status", "--porcelain"); dirty != "" {
		problems = append(problems, "the working tree has uncommitted changes")
	}
	toml, err := os.ReadFile(filepath.Join(root, "Cargo.toml"))
	if err != nil {
		problems = append(problems, err.Error())
	} else if m := cargoVersion.FindSubmatch(toml); m == nil {
		problems = append(problems, "Cargo.toml has no version")
	} else if want := strings.TrimPrefix(name, "a"); string(m[1]) != want {
		problems = append(problems, fmt.Sprintf("Cargo.toml says %s, but this release needs %s", m[1], want))
	}
	changelog, err := os.ReadFile(filepath.Join(releaseDir(root, name), "CHANGELOG.md"))
	if err != nil || len(bytes.TrimSpace(changelog)) == 0 {
		problems = append(problems, "releases/"+name+"/CHANGELOG.md is missing or empty")
	}
	return problems
}

// cmdRelease tests the committed state, then tags it. It never pushes.
func cmdRelease(root, name string) error {
	if problems := checkRelease(root, name); len(problems) > 0 {
		return fmt.Errorf("can't release %s:\n  - %s", name, strings.Join(problems, "\n  - "))
	}
	if _, err := os.Stat(filepath.Join(releaseDir(root, name), "benchmark.txt")); err != nil {
		fmt.Printf("note: no benchmark for %s yet; `play snapshot %s` records one\n", name, name)
	}
	out := os.Stdout
	if err := runVisible(root, out, devShell("cargo", "test", "--release")...); err != nil {
		return fmt.Errorf("tests failed: %w", err)
	}
	link := buildLink(root, name)
	if err := os.MkdirAll(filepath.Dir(link), 0o755); err != nil {
		return err
	}
	if err := runVisible(root, out, "nix", "build", ".#default", "--print-build-logs", "--out-link", link); err != nil {
		return fmt.Errorf("package build failed: %w", err)
	}
	// The end-to-end test runs on the packaged game, driver wrapper included.
	if err := runVisible(root, out, gameBinary(root, name), "--headless"); err != nil {
		return fmt.Errorf("end-to-end test failed: %w", err)
	}
	if err := runVisible(root, out, "git", "tag", "-a", name, "-m", "Deepdelve "+name); err != nil {
		return err
	}
	// Building from the tag proves the release can be rebuilt later. Nix reuses the build.
	s := buildSteps(root, name)[0]
	if err := runVisible(root, out, s.argv...); err != nil {
		return fmt.Errorf("tagged %s, but building from the tag failed: %w", name, err)
	}
	fmt.Printf("\nReleased %s locally. To publish it:\n  git push origin HEAD %s\n", name, name)
	return nil
}

// cmdSnapshot records the working tree's benchmark and fixed screenshot into the release
// folder, and adds any other screenshots given. Commit the results before releasing.
func cmdSnapshot(root, name string, extras []string) error {
	if _, ok := parseVersion(name); !ok {
		return fmt.Errorf("%q is not a version name like a0.1.0", name)
	}
	shots := filepath.Join(releaseDir(root, name), "screenshots")
	if err := os.MkdirAll(shots, 0o755); err != nil {
		return err
	}
	out := os.Stdout
	if err := runVisible(root, out, devShell("cargo", "build", "--release")...); err != nil {
		return err
	}
	bin := filepath.Join(root, "target", "release", "deepdelve")

	tmp, err := os.MkdirTemp("", "deepdelve-snapshot")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	fixed := filepath.Join(tmp, "fixed-view.png")
	if err := runVisible(root, out, devShell(bin, "--headless", "--seed", snapshotSeed, "--size", snapshotSize,
		"--shot", fixed+"@"+snapshotView)...); err != nil {
		return err
	}
	for _, path := range append([]string{fixed}, extras...) {
		if err := compressScreenshot(path, shots); err != nil {
			return fmt.Errorf("%s: %w", path, err)
		}
	}

	var bench bytes.Buffer
	args := []string{"--headless", "--seed", snapshotSeed, "--size", snapshotSize, "--bench", "20", "--bench-speed", "120"}
	fmt.Fprintf(&bench, "$ deepdelve %s\n", strings.Join(args, " "))
	if err := runVisible(root, io.MultiWriter(out, &bench), devShell(append([]string{bin}, args...)...)...); err != nil {
		return err
	}
	path := filepath.Join(releaseDir(root, name), "benchmark.txt")
	if err := os.WriteFile(path, []byte(cleanReport(bench.String())), 0o644); err != nil {
		return err
	}
	fmt.Printf("\nSaved the benchmark and screenshots in releases/%s/\n", name)
	return nil
}

// cleanReport keeps a benchmark to what the game printed, without the tooling around it.
func cleanReport(report string) string {
	var keep []string
	for _, line := range strings.Split(report, "\n") {
		if line == "" || strings.HasPrefix(line, "==> ") || strings.HasPrefix(line, "warning: ") {
			continue
		}
		keep = append(keep, line)
	}
	return strings.Join(keep, "\n") + "\n"
}

// compressScreenshot writes a smaller JPEG into dir, since anything committed stays in
// the repository's history forever.
func compressScreenshot(path, dir string) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()
	src, _, err := image.Decode(f)
	if err != nil {
		return err
	}
	b := src.Bounds()
	w, h := b.Dx(), b.Dy()
	if w > screenshotWidth {
		h = h * screenshotWidth / w
		w = screenshotWidth
	}
	dst := image.NewRGBA(image.Rect(0, 0, w, h))
	draw.CatmullRom.Scale(dst, dst.Bounds(), src, b, draw.Src, nil)

	stem := strings.TrimSuffix(filepath.Base(path), filepath.Ext(path))
	out, err := os.Create(filepath.Join(dir, stem+".jpg"))
	if err != nil {
		return err
	}
	defer out.Close()
	return jpeg.Encode(out, dst, &jpeg.Options{Quality: screenshotQuality})
}
