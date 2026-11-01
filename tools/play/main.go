// Command play runs and releases versions of Deepdelve. See docs/RELEASES.md.
package main

import (
	"fmt"
	"os"

	tea "github.com/charmbracelet/bubbletea"
)

const usage = `play                                  choose a version to run
play snapshot VERSION [IMAGE.png...]  record a benchmark and screenshots for a release
play release VERSION                  test the committed state and tag it
`

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "play:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	root, err := findRoot()
	if err != nil {
		return err
	}
	switch {
	case len(args) == 0:
		p := tea.NewProgram(newModel(root), tea.WithAltScreen(), tea.WithMouseCellMotion())
		_, err := p.Run()
		return err
	case args[0] == "release" && len(args) == 2:
		return cmdRelease(root, args[1])
	case args[0] == "snapshot" && len(args) >= 2:
		return cmdSnapshot(root, args[1], args[2:])
	default:
		fmt.Print(usage)
		if args[0] == "help" || args[0] == "--help" || args[0] == "-h" {
			return nil
		}
		return fmt.Errorf("unknown arguments")
	}
}
