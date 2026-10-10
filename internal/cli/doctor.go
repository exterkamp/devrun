package cli

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"

	"github.com/exterkamp/devrun/internal/herdr"
)

func init() {
	Register(Command{Name: "doctor", Summary: "check herdr, state dir, port range: doctor [--json]", Run: cmdDoctor})
}

// Check levels. Only Fail makes doctor exit non-zero.
const (
	levelOK   = "ok"
	levelWarn = "warn"
	levelFail = "FAIL"
)

// Check is one doctor finding.
type Check struct {
	Name   string `json:"name"`
	Level  string `json:"level"`
	Detail string `json:"detail"`
}

func cmdDoctor(env *Env, args []string) error {
	fs := newFlagSet(env, "doctor")
	asJSON := fs.Bool("json", false, "machine-readable output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	checks := runDoctor(env)
	failed := false
	for _, c := range checks {
		if c.Level == levelFail {
			failed = true
		}
	}
	if *asJSON {
		enc := json.NewEncoder(env.Stdout)
		enc.SetIndent("", "  ")
		if err := enc.Encode(checks); err != nil {
			return err
		}
	} else {
		color := useColor(env.Stdout)
		for _, c := range checks {
			code := ansiGreen
			switch c.Level {
			case levelWarn:
				code = ansiYellow
			case levelFail:
				code = ansiRed
			}
			fmt.Fprintf(env.Stdout, "%s  %-12s %s\n", paint(color, code, fmt.Sprintf("%-4s", c.Level)), c.Name, c.Detail)
		}
	}
	if failed {
		return exitError{code: 1}
	}
	return nil
}

func runDoctor(env *Env) []Check {
	var cs []Check
	add := func(name, level, format string, a ...interface{}) {
		cs = append(cs, Check{Name: name, Level: level, Detail: fmt.Sprintf(format, a...)})
	}

	// herdr is optional: without it notify falls back to stderr.
	bin, ok := herdr.Binary()
	switch {
	case !ok:
		add("herdr", levelWarn, "not found; notify will fall back to stderr (set HERDR_BIN_PATH or add herdr to PATH)")
	default:
		v, err := herdr.Version()
		if err != nil {
			add("herdr", levelWarn, "%s: cannot run --version: %v", bin, err)
		} else {
			add("herdr", levelOK, "%s (%s)", bin, v)
		}
		if s, err := herdr.GetSnapshot(); err != nil {
			if herdr.ServerNotRunning(err) {
				add("herdr-server", levelWarn, "no herdr server running; notifications will fall back to stderr")
			} else {
				add("herdr-server", levelWarn, "%v", err)
			}
		} else {
			add("herdr-server", levelOK, "reachable, version %s, protocol %d", s.Version, s.Protocol)
		}
	}
	switch {
	case herdr.InPane():
		add("herdr-pane", levelOK, "running in pane %s (workspace %s, tab %s)",
			os.Getenv("HERDR_PANE_ID"), os.Getenv("HERDR_WORKSPACE_ID"), os.Getenv("HERDR_TAB_ID"))
	case os.Getenv("HERDR_ENV") == "1":
		add("herdr-pane", levelWarn, "HERDR_ENV=1 but HERDR_PANE_ID is empty; owner will not be recorded")
	default:
		add("herdr-pane", levelWarn, "not inside a herdr pane (HERDR_ENV unset); owner not recorded, notify prints to stderr")
	}

	// State dir must be writable.
	if err := os.MkdirAll(env.Store.LogDir(), 0o755); err != nil {
		add("state-dir", levelFail, "%s: %v", env.Store.Dir, err)
	} else if f, err := os.CreateTemp(env.Store.Dir, ".doctor-*"); err != nil {
		add("state-dir", levelFail, "%s not writable: %v", env.Store.Dir, err)
	} else {
		f.Close()
		os.Remove(f.Name())
		add("state-dir", levelOK, "%s writable", env.Store.Dir)
	}

	// Check the native listener backend.
	listening, lerr := env.Listening()
	if lerr != nil {
		add("listeners", levelFail, "cannot read listening ports: %v", lerr)
	} else {
		add("listeners", levelOK, "%d listening port(s) readable", len(listening))
	}
	if runtime.GOOS == "darwin" {
		if _, err := exec.LookPath("/usr/sbin/lsof"); err != nil {
			add("lsof", levelFail, "missing /usr/sbin/lsof (required on macOS)")
		} else {
			add("lsof", levelOK, "available (macOS native listener detection)")
		}
	} else if _, err := exec.LookPath("ss"); err != nil {
		add("ss", levelWarn, "not found (only a fallback; /proc/net/tcp is used)")
	} else {
		add("ss", levelOK, "available")
	}

	// Port range: report foreign listeners in it.
	if lerr == nil && env.Alloc != nil {
		var foreign []int
		f, err := env.Store.Load()
		if err == nil {
			mine := f.PortsInUse("")
			for p := range listening {
				if p >= env.Alloc.Min && p <= env.Alloc.Max && !mine[p] {
					foreign = append(foreign, p)
				}
			}
		}
		sort.Ints(foreign)
		if len(foreign) > 0 {
			add("port-range", levelWarn, "%d-%d: %d port(s) in use by non-devrun processes: %v",
				env.Alloc.Min, env.Alloc.Max, len(foreign), foreign)
		} else {
			add("port-range", levelOK, "%d-%d free of foreign listeners", env.Alloc.Min, env.Alloc.Max)
		}
	}

	if _, err := exec.LookPath("git"); err != nil {
		add("git", levelFail, "not found on PATH")
	} else {
		add("git", levelOK, "available")
	}
	if p, err := exec.LookPath("docker"); err != nil {
		add("docker", levelWarn, "not found (only needed for --compose)")
	} else {
		add("docker", levelOK, "%s", filepath.Clean(p))
	}
	return cs
}
