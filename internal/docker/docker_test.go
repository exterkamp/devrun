package docker

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/exterkamp/devrun/internal/registry"
	"github.com/exterkamp/devrun/internal/runner"
)

// fakeDocker installs a `docker` script on PATH that records its argv and key
// env vars to a log and answers from files in the same directory:
//
//	ps.json         -> output of `docker compose ... ps`
//	published.txt   -> output of `docker ps`
//	fail            -> if present, `compose ... up` exits 1
func fakeDocker(t *testing.T) (dir string, calls func() []string) {
	t.Helper()
	dir = t.TempDir()
	script := `#!/bin/sh
d="$(dirname "$0")"
echo "$* | WEB_PORT=$WEB_PORT PORT=$PORT CPN=$COMPOSE_PROJECT_NAME PWD=$(pwd)" >> "$d/calls.log"
if [ "$1" = compose ]; then
  case "$*" in
    *" ps "*) cat "$d/ps.json" 2>/dev/null; exit 0 ;;
    *" up "*) [ -e "$d/fail" ] && { echo "up exploded" >&2; exit 1; }; exit 0 ;;
    *" logs "*) echo "log line"; exit 0 ;;
    *) exit 0 ;;
  esac
fi
if [ "$1" = ps ]; then cat "$d/published.txt" 2>/dev/null; exit 0; fi
exit 0
`
	if err := os.WriteFile(filepath.Join(dir, "docker"), []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	return dir, func() []string {
		b, _ := os.ReadFile(filepath.Join(dir, "calls.log"))
		return strings.Split(strings.TrimSpace(string(b)), "\n")
	}
}

func entry(t *testing.T) registry.Entry {
	wt := t.TempDir()
	return registry.Entry{
		Key: "my-app:" + wt + ":web", Project: "My-App", Worktree: wt, Name: "web", Kind: Kind,
		Cmd:   []string{"docker", "compose", "-f", wt + "/compose.yml", "up", "-d", "--wait"},
		Ports: map[string]int{"WEB_PORT": 41237}, PrimaryPort: "WEB_PORT",
		Log: filepath.Join(t.TempDir(), "x.log"),
	}
}

func TestProjectNameAndPortVar(t *testing.T) {
	a, b := ProjectName("My App", "/w/a"), ProjectName("My App", "/w/b")
	if a == b || !strings.HasPrefix(a, "devrun-my-app-") || a != ProjectName("My App", "/w/a") {
		t.Fatalf("%s %s", a, b)
	}
	if len(a) != len("devrun-my-app-")+6 {
		t.Fatalf("hash length: %s", a)
	}
	if PortVar("web") != "WEB_PORT" || PortVar("my-api") != "MY_API_PORT" {
		t.Fatal(PortVar("my-api"))
	}
	if SafeProject("other-proj") == nil || SafeProject("devrun-") == nil || SafeProject(a) != nil {
		t.Fatal("SafeProject")
	}
}

func TestParseComposePS(t *testing.T) {
	arr := `[{"Name":"a","Service":"web","State":"running"},{"Name":"b","Service":"db","State":"exited"}]`
	nd := `{"Name":"a","State":"exited"}` + "\n" + `{"Name":"b","State":"running","Health":"healthy"}`
	for _, in := range []string{arr, nd} {
		cs, err := ParseComposePS([]byte(in))
		if err != nil || len(cs) != 2 || !Running(cs) {
			t.Fatalf("%v %v", cs, err)
		}
	}
	cs, _ := ParseComposePS(nil)
	if Running(cs) {
		t.Fatal("empty is not running")
	}
	cs, _ = ParseComposePS([]byte(`[{"State":"exited"}]`))
	if Running(cs) {
		t.Fatal("exited is not running")
	}
}

func TestParsePublished(t *testing.T) {
	got := ParsePublished("0.0.0.0:8083->80/tcp, :::8083->80/tcp\n127.0.0.1:4200->4200/tcp, 3000/tcp\n0.0.0.0:9000-9002->9000-9002/tcp\n")
	for _, p := range []int{8083, 4200, 9000, 9001, 9002} {
		if !got[p] {
			t.Errorf("missing %d", p)
		}
	}
	if got[80] || got[3000] || len(got) != 5 {
		t.Fatalf("%v", got)
	}
}

func TestPublishedPortsUsesDockerPs(t *testing.T) {
	dir, _ := fakeDocker(t)
	os.WriteFile(filepath.Join(dir, "published.txt"), []byte("0.0.0.0:41500->80/tcp\n"), 0o644)
	got, err := PublishedPorts()
	if err != nil || !got[41500] {
		t.Fatalf("%v %v", got, err)
	}
}

func TestPublishedPortsDegradesWithoutDocker(t *testing.T) {
	t.Setenv("PATH", t.TempDir())
	got, err := PublishedPorts()
	if err != nil || len(got) != 0 {
		t.Fatalf("%v %v", got, err)
	}
}

func TestPublishedPortsDegradesOnDockerError(t *testing.T) {
	dir := t.TempDir()
	os.WriteFile(filepath.Join(dir, "docker"), []byte("#!/bin/sh\nexit 1\n"), 0o755)
	t.Setenv("PATH", dir)
	if got, err := PublishedPorts(); err != nil || len(got) != 0 {
		t.Fatalf("%v %v", got, err)
	}
}

func TestStartLaunchesComposeUpWithEnv(t *testing.T) {
	dir, calls := fakeDocker(t)
	e := entry(t)
	h, err := (Runner{}).Start(runner.Spec{Entry: e, Dir: e.Worktree, Primary: "WEB_PORT"})
	if err != nil {
		t.Fatal(err)
	}
	want := ProjectName("My-App", e.Worktree)
	if h.ComposeProject != want || h.PID == 0 {
		t.Fatalf("%+v", h)
	}
	var line string
	for i := 0; i < 100 && line == ""; i++ {
		time.Sleep(20 * time.Millisecond)
		for _, c := range calls() {
			if strings.Contains(c, " up ") {
				line = c
			}
		}
	}
	_ = dir
	exp := "compose -p " + want + " -f " + e.Worktree + "/compose.yml up -d --wait"
	if !strings.HasPrefix(line, exp) {
		t.Fatalf("argv: %q want prefix %q", line, exp)
	}
	if !strings.Contains(line, "WEB_PORT=41237") || !strings.Contains(line, "CPN="+want) {
		t.Fatalf("env: %q", line)
	}
	physicalWorktree, err := filepath.EvalSymlinks(e.Worktree)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(line, "PWD="+physicalWorktree) {
		t.Fatalf("cwd: %q", line)
	}
}

func TestStartRejectsBadCmd(t *testing.T) {
	fakeDocker(t)
	e := entry(t)
	e.Cmd = []string{"rm", "-rf", "/"}
	if _, err := (Runner{}).Start(runner.Spec{Entry: e, Dir: e.Worktree}); err == nil {
		t.Fatal("expected error")
	}
}

func TestAliveUsesComposePS(t *testing.T) {
	dir, calls := fakeDocker(t)
	e := entry(t)
	e.ComposeProject = ProjectName(e.Project, e.Worktree)
	if (Runner{}).Alive(e) {
		t.Fatal("no containers: not alive")
	}
	os.WriteFile(filepath.Join(dir, "ps.json"), []byte(`[{"Name":"c","Service":"web","State":"running"}]`), 0o644)
	if !(Runner{}).Alive(e) {
		t.Fatal("running container: alive")
	}
	last := calls()[len(calls())-1]
	if !strings.HasPrefix(last, "compose -p "+e.ComposeProject+" -f "+e.Worktree+"/compose.yml ps --format json") {
		t.Fatalf("argv %q", last)
	}
	os.WriteFile(filepath.Join(dir, "ps.json"), []byte(`[{"State":"exited"}]`), 0o644)
	if (Runner{}).Alive(e) {
		t.Fatal("exited: not alive")
	}
	e.ComposeProject = "someone-elses"
	os.WriteFile(filepath.Join(dir, "ps.json"), []byte(`[{"State":"running"}]`), 0o644)
	if (Runner{}).Alive(e) {
		t.Fatal("non-devrun project must never count")
	}
}

func TestStopRunsDownWithoutVolumes(t *testing.T) {
	_, calls := fakeDocker(t)
	e := entry(t)
	e.ComposeProject = ProjectName(e.Project, e.Worktree)
	if err := (Runner{}).Stop(e, 5*time.Second); err != nil {
		t.Fatal(err)
	}
	var down string
	for _, c := range calls() {
		if strings.Contains(c, " down") {
			down = c
		}
		if strings.Contains(c, "volume") || strings.Contains(c, " -v") || strings.Contains(c, "prune") || strings.Contains(c, "rm ") {
			t.Fatalf("dangerous call: %q", c)
		}
	}
	if !strings.HasPrefix(down, "compose -p "+e.ComposeProject+" -f ") || !strings.Contains(down, " down --timeout 5 |") {
		t.Fatalf("down: %q", down)
	}
}

func TestStopRefusesForeignProject(t *testing.T) {
	_, calls := fakeDocker(t)
	e := entry(t)
	e.ComposeProject = "myapp-dev"
	if err := (Runner{}).Stop(e, time.Second); err == nil {
		t.Fatal("expected refusal")
	}
	if len(calls()) > 1 || (len(calls()) == 1 && calls()[0] != "") {
		t.Fatalf("docker was invoked: %v", calls())
	}
}

func TestLogsMapToComposeLogs(t *testing.T) {
	_, calls := fakeDocker(t)
	e := entry(t)
	e.ComposeProject = ProjectName(e.Project, e.Worktree)
	var out bytes.Buffer
	if err := (Runner{}).Logs(context.Background(), e, runner.LogOpts{Follow: true, Lines: 50}, &out); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "log line") {
		t.Fatal(out.String())
	}
	last := calls()[len(calls())-1]
	if !strings.Contains(last, " logs --no-color --tail 50 --follow |") {
		t.Fatalf("%q", last)
	}
}
