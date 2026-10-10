// Package registry persists the set of servers devrun owns.
//
// The registry is a single JSON file guarded by an flock lockfile and written
// atomically (tmp file + rename), so concurrent devrun invocations never see a
// torn file and never lose each other's updates.
package registry

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"syscall"
	"time"
)

const fileVersion = 1

// Owner identifies who started a server (filled in by later milestones).
type Owner struct {
	HerdrPane      string `json:"herdr_pane,omitempty"`
	HerdrWorkspace string `json:"herdr_workspace,omitempty"`
	HerdrTab       string `json:"herdr_tab,omitempty"`
	Agent          string `json:"agent,omitempty"`
}

// Flag marks a server as needing human inspection.
type Flag struct {
	At      time.Time `json:"at"`
	Message string    `json:"message"`
}

// Entry is one registered server. Field names are part of the --json contract.
type Entry struct {
	Key            string         `json:"key"`
	Project        string         `json:"project"`
	Worktree       string         `json:"worktree"`
	Branch         string         `json:"branch"`
	Name           string         `json:"name"`
	Kind           string         `json:"kind"`
	PID            int            `json:"pid"`
	PGID           int            `json:"pgid"`
	PIDStart       uint64         `json:"pid_start"` // platform process start time, guards against PID reuse
	Cmd            []string       `json:"cmd"`
	Ports          map[string]int `json:"ports"`
	PrimaryPort    string         `json:"primary_port,omitempty"` // key into Ports used for URL
	ComposeProject string         `json:"compose_project,omitempty"`
	StartedAt      time.Time      `json:"started_at"`
	Owner          *Owner         `json:"owner,omitempty"`
	Flagged        *Flag          `json:"flagged,omitempty"`
	Log            string         `json:"log"`
}

// Key builds the registry key for a (project, worktree, name) triple.
func Key(project, worktree, name string) string {
	return project + ":" + worktree + ":" + name
}

// Port returns the port used for the entry's URL: the PrimaryPort if set,
// else "PORT", else the alphabetically first name. Zero if there are none.
func (e *Entry) Port() int {
	if p, ok := e.Ports[e.PrimaryPort]; ok && e.PrimaryPort != "" {
		return p
	}
	if p, ok := e.Ports["PORT"]; ok {
		return p
	}
	names := make([]string, 0, len(e.Ports))
	for n := range e.Ports {
		names = append(names, n)
	}
	sort.Strings(names)
	if len(names) == 0 {
		return 0
	}
	return e.Ports[names[0]]
}

// URL is the address an agent or browser should use.
func (e *Entry) URL() string {
	p := e.Port()
	if p == 0 {
		return ""
	}
	return fmt.Sprintf("http://127.0.0.1:%d", p)
}

// File is the on-disk document.
type File struct {
	Version int      `json:"version"`
	Entries []*Entry `json:"entries"`
	// Reserved remembers the port handed to "<key>#<portname>" so a worktree
	// keeps its port across stop/up cycles.
	Reserved map[string]int `json:"reserved,omitempty"`
}

// Find returns the entry with the given key, or nil.
func (f *File) Find(key string) *Entry {
	for _, e := range f.Entries {
		if e.Key == key {
			return e
		}
	}
	return nil
}

// Upsert replaces the entry with the same key, or appends it.
func (f *File) Upsert(e *Entry) {
	for i, old := range f.Entries {
		if old.Key == e.Key {
			f.Entries[i] = e
			return
		}
	}
	f.Entries = append(f.Entries, e)
}

// Remove deletes the entry with the given key and reports whether it existed.
func (f *File) Remove(key string) bool {
	for i, e := range f.Entries {
		if e.Key == key {
			f.Entries = append(f.Entries[:i], f.Entries[i+1:]...)
			return true
		}
	}
	return false
}

// PortsInUse returns every port held by an entry other than excludeKey.
func (f *File) PortsInUse(excludeKey string) map[int]bool {
	used := map[int]bool{}
	for _, e := range f.Entries {
		if e.Key == excludeKey {
			continue
		}
		for _, p := range e.Ports {
			used[p] = true
		}
	}
	return used
}

// Store is a registry rooted at a state directory.
type Store struct {
	Dir string
}

// DefaultDir resolves the state directory: $DEVRUN_STATE_DIR, else
// $XDG_STATE_HOME/devrun, else ~/.local/state/devrun.
func DefaultDir() (string, error) {
	if d := os.Getenv("DEVRUN_STATE_DIR"); d != "" {
		return d, nil
	}
	if d := os.Getenv("XDG_STATE_HOME"); d != "" {
		return filepath.Join(d, "devrun"), nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".local", "state", "devrun"), nil
}

// Path is the registry file location.
func (s *Store) Path() string { return filepath.Join(s.Dir, "registry.json") }

// LogDir is where per-server log files live.
func (s *Store) LogDir() string { return filepath.Join(s.Dir, "logs") }

func (s *Store) lockPath() string { return filepath.Join(s.Dir, "registry.lock") }

func (s *Store) lock(how int) (*os.File, error) {
	if err := os.MkdirAll(s.Dir, 0o755); err != nil {
		return nil, err
	}
	lf, err := os.OpenFile(s.lockPath(), os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, err
	}
	for {
		err = syscall.Flock(int(lf.Fd()), how)
		if err != syscall.EINTR {
			break
		}
	}
	if err != nil {
		lf.Close()
		return nil, err
	}
	return lf, nil
}

func (s *Store) read() (*File, error) {
	data, err := os.ReadFile(s.Path())
	if errors.Is(err, os.ErrNotExist) {
		return &File{Version: fileVersion}, nil
	}
	if err != nil {
		return nil, err
	}
	f := &File{}
	if len(data) > 0 {
		if err := json.Unmarshal(data, f); err != nil {
			return nil, fmt.Errorf("registry %s is corrupt: %w", s.Path(), err)
		}
	}
	f.Version = fileVersion
	return f, nil
}

func (s *Store) write(f *File) error {
	data, err := json.MarshalIndent(f, "", "  ")
	if err != nil {
		return err
	}
	tmp, err := os.CreateTemp(s.Dir, "registry-*.tmp")
	if err != nil {
		return err
	}
	name := tmp.Name()
	cleanup := func() { tmp.Close(); os.Remove(name) }
	if _, err := tmp.Write(data); err != nil {
		cleanup()
		return err
	}
	if err := tmp.Sync(); err != nil {
		cleanup()
		return err
	}
	if err := tmp.Close(); err != nil {
		os.Remove(name)
		return err
	}
	if err := os.Chmod(name, 0o644); err != nil {
		os.Remove(name)
		return err
	}
	if err := os.Rename(name, s.Path()); err != nil {
		os.Remove(name)
		return err
	}
	return nil
}

// Load reads a consistent snapshot under a shared lock.
func (s *Store) Load() (*File, error) {
	lf, err := s.lock(syscall.LOCK_SH)
	if err != nil {
		return nil, err
	}
	defer lf.Close() // closing releases the flock
	return s.read()
}

// Update runs fn on the current registry under an exclusive lock and writes
// the result back atomically if fn returns nil. Keep fn short: it blocks every
// other devrun invocation.
func (s *Store) Update(fn func(*File) error) error {
	lf, err := s.lock(syscall.LOCK_EX)
	if err != nil {
		return err
	}
	defer lf.Close()
	f, err := s.read()
	if err != nil {
		return err
	}
	if err := fn(f); err != nil {
		return err
	}
	return s.write(f)
}
