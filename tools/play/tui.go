package main

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/charmbracelet/bubbles/spinner"
	"github.com/charmbracelet/bubbles/textinput"
	"github.com/charmbracelet/bubbles/viewport"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

const (
	listWidth = 34
	maxLog    = 5000
)

var (
	amber   = lipgloss.Color("#E8B95A")
	teal    = lipgloss.Color("#5FD6B8")
	red     = lipgloss.Color("#E8695A")
	dim     = lipgloss.Color("#7C8A85")
	border  = lipgloss.Color("#3C4A46")
	titleS  = lipgloss.NewStyle().Foreground(amber).Bold(true)
	headS   = lipgloss.NewStyle().Foreground(teal).Bold(true)
	dimS    = lipgloss.NewStyle().Foreground(dim)
	readyS  = lipgloss.NewStyle().Foreground(teal)
	waitS   = lipgloss.NewStyle().Foreground(amber)
	errS    = lipgloss.NewStyle().Foreground(red)
	cursorS = lipgloss.NewStyle().Foreground(amber).Bold(true)
	panelS  = lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(border).Padding(0, 1)
)

type linesMsg []string
type jobDoneMsg struct{}

// waitForJob delivers a batch of output, so a fast build doesn't redraw for every line.
func waitForJob(j *job) tea.Cmd {
	return func() tea.Msg {
		line, ok := <-j.lines
		if !ok {
			return jobDoneMsg{}
		}
		batch := linesMsg{line}
		for len(batch) < 512 {
			select {
			case line, ok := <-j.lines:
				if !ok {
					return batch
				}
				batch = append(batch, line)
			default:
				return batch
			}
		}
		return batch
	}
}

type model struct {
	root     string
	names    []string // currentName first, then releases, newest first
	cursor   int
	settings settings

	pane    viewport.Model
	showLog bool
	log     []string

	job      *job
	jobName  string
	runsGame bool
	quitting bool

	status    string
	statusBad bool
	dirty     bool // cached, since the list redraws many times a second

	spin    spinner.Model
	input   textinput.Model
	editing bool

	width, height int
}

func newModel(root string) model {
	sp := spinner.New(spinner.WithSpinner(spinner.Dot), spinner.WithStyle(waitS))
	in := textinput.New()
	in.Prompt = ""
	in.Placeholder = "--seed 777 --no-vsync"
	m := model{
		root:     root,
		settings: loadSettings(root),
		pane:     viewport.New(0, 0),
		spin:     sp,
		input:    in,
	}
	m.names = append([]string{currentName}, listReleases(root)...)
	m.checkDirty()
	return m
}

func (m model) Init() tea.Cmd { return nil }

func (m model) selected() string { return m.names[m.cursor] }

func (m *model) layout() {
	m.pane.Width = max(m.width-listWidth-6, 10)
	m.pane.Height = max(m.height-6, 3)
	m.refreshPane()
}

func (m *model) refreshPane() {
	wrap := lipgloss.NewStyle().Width(m.pane.Width)
	if m.showLog {
		follow := m.pane.AtBottom()
		m.pane.SetContent(wrap.Render(strings.Join(m.log, "\n")))
		if follow {
			m.pane.GotoBottom()
		}
		return
	}
	m.pane.SetContent(wrap.Render(m.preview(m.selected())))
	m.pane.GotoTop()
}

func (m model) launchArgs(name string) []string {
	return strings.Fields(m.settings[name])
}

func (m *model) start(name string, steps []step, runsGame bool) tea.Cmd {
	m.job = startJob(m.root, steps)
	m.jobName = name
	m.runsGame = runsGame
	m.log = nil
	m.showLog = true
	m.status = ""
	m.statusBad = false
	m.refreshPane()
	m.pane.GotoBottom()
	return tea.Batch(waitForJob(m.job), m.spin.Tick)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
		m.layout()
		return m, nil

	case spinner.TickMsg:
		if m.job == nil {
			return m, nil
		}
		var cmd tea.Cmd
		m.spin, cmd = m.spin.Update(msg)
		return m, cmd

	case linesMsg:
		m.log = append(m.log, msg...)
		if over := len(m.log) - maxLog; over > 0 {
			m.log = m.log[over:]
		}
		m.refreshPane()
		return m, waitForJob(m.job)

	case jobDoneMsg:
		m.finishJob()
		if m.quitting {
			return m, tea.Quit
		}
		return m, nil

	case tea.KeyMsg:
		if m.editing {
			return m.updateEditing(msg)
		}
		return m.updateKey(msg)
	}

	var cmd tea.Cmd
	m.pane, cmd = m.pane.Update(msg)
	return m, cmd
}

func (m *model) checkDirty() {
	m.dirty = git(m.root, "status", "--porcelain") != ""
}

func (m *model) finishJob() {
	err := m.job.err
	name := m.jobName
	m.job = nil
	m.checkDirty()
	m.statusBad = false
	switch {
	case errors.Is(err, errStopped):
		m.status = name + " stopped"
	case err != nil:
		m.status = fmt.Sprintf("%s failed: %v", name, err)
		m.statusBad = true
	case m.runsGame:
		m.status = name + " closed"
	default:
		m.status = name + " is built"
	}
}

func (m model) updateEditing(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.Type {
	case tea.KeyEsc:
		m.editing = false
		m.input.Blur()
		return m, nil
	case tea.KeyEnter:
		m.editing = false
		m.input.Blur()
		m.settings[m.selected()] = strings.TrimSpace(m.input.Value())
		if err := m.settings.save(m.root); err != nil {
			m.status, m.statusBad = "couldn't save options: "+err.Error(), true
		}
		return m, nil
	}
	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	return m, cmd
}

func (m model) updateKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	key := msg.String()
	if key == "ctrl+c" {
		if m.job == nil {
			return m, tea.Quit
		}
		m.quitting = true
		m.job.stop()
		return m, nil
	}
	if m.job != nil {
		if key == "s" || key == "esc" {
			m.status = "Stopping..."
			m.job.stop()
			return m, nil
		}
		var cmd tea.Cmd
		m.pane, cmd = m.pane.Update(msg)
		return m, cmd
	}

	name := m.selected()
	switch key {
	case "q":
		return m, tea.Quit
	case "up", "k":
		m.move(-1)
	case "down", "j":
		m.move(1)
	case "enter":
		return m, m.start(name, runSteps(m.root, name, m.launchArgs(name)), true)
	case "b":
		if name == currentName {
			return m, m.start(name, []step{{
				label: "Building the working tree",
				argv:  devShell("cargo", "build", "--release"),
			}}, false)
		}
		return m, m.start(name, buildSteps(m.root, name), false)
	case "d":
		if name != currentName {
			if err := os.RemoveAll(filepath.Dir(buildLink(m.root, name))); err != nil {
				m.status, m.statusBad = err.Error(), true
			} else {
				m.status, m.statusBad = "Removed the build of "+name+"; it rebuilds from source on the next run", false
			}
			m.showLog = false
			m.refreshPane()
		}
	case "a":
		m.editing = true
		m.input.SetValue(m.settings[name])
		m.input.CursorEnd()
		return m, m.input.Focus()
	case "l":
		if len(m.log) > 0 {
			m.showLog = !m.showLog
			m.refreshPane()
			if m.showLog {
				m.pane.GotoBottom()
			}
		}
	default:
		var cmd tea.Cmd
		m.pane, cmd = m.pane.Update(msg)
		return m, cmd
	}
	return m, nil
}

func (m *model) move(delta int) {
	next := m.cursor + delta
	if next < 0 || next >= len(m.names) {
		return
	}
	m.cursor = next
	m.checkDirty()
	m.showLog = false
	m.status = ""
	m.statusBad = false
	m.refreshPane()
}

// preview describes a version for the right-hand pane.
func (m model) preview(name string) string {
	var b strings.Builder
	if name == currentName {
		m.previewCurrent(&b)
		return b.String()
	}
	date := git(m.root, "log", "-1", "--format=%ad", "--date=short", name)
	commit := git(m.root, "rev-parse", "--short", name+"^{commit}")
	b.WriteString(titleS.Render(name) + dimS.Render("   released "+date+"   commit "+commit) + "\n\n")

	dir := releaseDir(m.root, name)
	changelog := readReleaseFile(m.root, name, "CHANGELOG.md")
	if changelog == "" {
		b.WriteString(dimS.Render("This release has no changelog.") + "\n")
	}
	for _, line := range strings.Split(strings.TrimSpace(changelog), "\n") {
		if strings.HasPrefix(line, "#") {
			line = headS.Render(strings.TrimLeft(line, "# "))
		}
		b.WriteString(line + "\n")
	}
	if bench := benchSummary(readReleaseFile(m.root, name, "benchmark.txt")); bench != "" {
		b.WriteString("\n" + headS.Render("Benchmark") + "\n" + bench + "\n")
	}
	if shots, _ := filepath.Glob(filepath.Join(dir, "screenshots", "*")); len(shots) > 0 {
		rel, _ := filepath.Rel(m.root, filepath.Join(dir, "screenshots"))
		b.WriteString("\n" + headS.Render("Screenshots") + "\n")
		b.WriteString(strconv.Itoa(len(shots)) + " in " + rel + "/\n")
	}
	return b.String()
}

func (m model) previewCurrent(b *strings.Builder) {
	branch := git(m.root, "branch", "--show-current")
	commit := git(m.root, "rev-parse", "--short", "HEAD")
	b.WriteString(titleS.Render("Working tree") + dimS.Render("   "+branch+" at "+commit) + "\n\n")
	b.WriteString("Runs the code as it is right now, including uncommitted changes.\n\n")

	changed := git(m.root, "status", "--short")
	if changed == "" {
		b.WriteString(dimS.Render("No uncommitted changes.") + "\n")
	} else {
		b.WriteString(headS.Render("Uncommitted changes") + "\n" + changed + "\n")
	}
	if len(m.names) > 1 {
		latest := m.names[1]
		log := git(m.root, "log", "--oneline", latest+"..HEAD")
		b.WriteString("\n" + headS.Render("Commits since "+latest) + "\n")
		if log == "" {
			log = dimS.Render("None.")
		}
		b.WriteString(log + "\n")
	}
}

// readReleaseFile prefers the working tree, so a changelog can be corrected after its
// release, and falls back to the file at the release's tag.
func readReleaseFile(root, name, file string) string {
	if data, err := os.ReadFile(filepath.Join(releaseDir(root, name), file)); err == nil {
		return string(data)
	}
	return git(root, "show", name+":releases/"+name+"/"+file)
}

// benchSummary picks the lines worth comparing out of a benchmark report.
func benchSummary(report string) string {
	var keep []string
	for _, line := range strings.Split(report, "\n") {
		t := strings.TrimSpace(line)
		if strings.HasPrefix(t, "bench:") || strings.HasPrefix(t, "frame ms:") || strings.HasPrefix(t, "gpu ms:") ||
			strings.HasPrefix(t, "world ready") {
			keep = append(keep, t)
		}
	}
	return strings.Join(keep, "\n")
}

func (m model) View() string {
	if m.width == 0 {
		return ""
	}
	title := titleS.Render("DEEPDELVE") + dimS.Render("  choose a version")
	left := panelS.Width(listWidth).Height(m.pane.Height + 1).Render(m.viewList())
	heading := "Details"
	if m.showLog {
		heading = "Output"
	}
	right := panelS.Width(m.pane.Width + 2).Height(m.pane.Height + 1).
		Render(headS.Render(heading) + "\n" + m.pane.View())
	return lipgloss.JoinVertical(lipgloss.Left,
		title,
		lipgloss.JoinHorizontal(lipgloss.Top, left, right),
		m.viewStatus(),
		dimS.Render(m.helpText()),
	)
}

func (m model) viewList() string {
	var b strings.Builder
	for i, name := range m.names {
		marker, label := "  ", name
		if name == currentName {
			label = "Build and run current"
		}
		if i == m.cursor {
			marker = cursorS.Render("> ")
			label = cursorS.Render(label)
		}
		b.WriteString(marker + label + "\n  " + m.viewItemStatus(name) + "\n")
		if name == currentName && len(m.names) > 1 {
			b.WriteString("\n")
		}
	}
	if len(m.names) == 1 {
		b.WriteString("\n" + dimS.Render("  No releases yet."))
	}
	return b.String()
}

func (m model) viewItemStatus(name string) string {
	if m.job != nil && m.jobName == name {
		return m.spin.View() + waitS.Render("working")
	}
	if name == currentName {
		if m.dirty {
			return waitS.Render("has uncommitted changes")
		}
		return dimS.Render("no uncommitted changes")
	}
	if isBuilt(m.root, name) {
		return readyS.Render("ready")
	}
	return waitS.Render("needs build")
}

func (m model) viewStatus() string {
	if m.editing {
		return headS.Render("Launch options for "+m.selected()+": ") + m.input.View()
	}
	if m.status != "" {
		if m.statusBad {
			return errS.Render(m.status)
		}
		return readyS.Render(m.status)
	}
	if opts := m.settings[m.selected()]; opts != "" {
		return dimS.Render("Launch options: " + opts)
	}
	return ""
}

func (m model) helpText() string {
	switch {
	case m.editing:
		return "enter save   esc cancel"
	case m.job != nil:
		return "s stop   pgup/pgdn scroll   ctrl+c stop and quit"
	case m.selected() == currentName:
		return "enter run   b build   a launch options   l last output   q quit"
	default:
		return "enter run   b build   d remove build   a launch options   l last output   q quit"
	}
}
