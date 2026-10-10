# devrun: spec

A small CLI that lets parallel coding agents (3-4 projects x 3-4 agents, each on its own git worktree) start dev servers without port collisions, so you can always see what is running, where, and who owns it, and so anything can be cleaned up.

Status: M1-M5 implemented; M6 is later. Decisions marked **[assumed]** were my defaults, so change them if wrong. Confirmed by the user: port range 41000-41999, no per-project cap, auto-stop when a worktree is removed, Docker/compose support in v1.

## Goals

1. **Allocate**: every (project, worktree) gets its own stable port. No hardcoded ports.
2. **Own**: every running server is recorded with project, worktree, branch, agent pane, PID, port and URL.
3. **See**: one command (and a live view for a herdr pane) shows everything, filterable by project.
4. **Clean up**: stop one server, one worktree, one project, or everything, including child processes.
5. **Notify without blocking**: an agent can tell you "inspect :PORT" via herdr and keep working.
6. **Agent-friendly**: every command has `--json`; a short CLAUDE.md/skill snippet teaches agents the workflow.

## Non-goals (v1)

- No MCP server. Agents call the CLI. An MCP wrapper can come later.
- No remote machines or Windows support. Linux and macOS (Apple Silicon and Intel) are supported.
- No Kubernetes, no Swarm; compose only.
- No reverse proxy or pretty hostnames.
- No log aggregation beyond per-server log files.

## Commands

| Command | Behaviour |
|---|---|
| `devrun up [--name web] [--port-env PORT] -- <cmd...>` | Resolve project and worktree from cwd (git toplevel, `git worktree`). Allocate or reuse the port for (worktree, name). Start `<cmd>` detached in its own process group with `PORT` set (and `{port}` substituted in args). Log to `~/.local/state/devrun/logs/`. Wait until the port accepts connections (`--wait 60s`), then print the URL. Exit non-zero if the process dies or never listens. If already running for this key, print the existing URL and exit 0 (idempotent). |
| `devrun up --compose [--ports web,api] [--port-var web=VAR] [--build] [-f file] [-- <extra compose args>]` | Docker mode. Allocate one port per name in `--ports` (default a single `PORT`). Export them as env vars (`PORT`, or `WEB_PORT`, `API_PORT`), set `COMPOSE_PROJECT_NAME=devrun-<project>-<worktree-hash>`, then run `docker compose up -d --wait` (`--build` adds `--build`). The compose file uses `${WEB_PORT}:3000` in its `ports:`. Registers the stack, waits for the allocated port(s) to accept connections, prints URL(s). Idempotent like process mode. |
| `devrun ls [--project P] [--json] [--all]` | Table: project, worktree/branch, name, port, URL, PID, kind (`proc`/`docker`), state (`starting`/`up`/`dead`; flagged rows are marked `!`), age, owner pane. Dead entries are hidden without `--all`. Port-drift and similar warnings go to stderr. Orphans are reported by `gc`, not `ls`. |
| `devrun watch [--project P] [--interval 1s] [--once]` | Live refreshing TUI of `ls`, for a herdr pane. Highlights newly flagged ports. |
| `devrun url [name]` | Print the URL for this worktree's server (for Playwright MCP). |
| `devrun logs [name] [-f] [-n lines]` | Tail the server log. |
| `devrun stop [target]` | Stop a server. `target` resolves as: name in this worktree, exact port, `project/name`, then name across all servers; an ambiguous match (or no target outside a git repo with several servers) lists the candidates and exits non-zero without acting. Process mode: SIGTERM the process group, SIGKILL after 5s. Docker mode: `docker compose -p <project> down`; volumes are never removed. Frees the port either way. |
| `devrun stop --project P` / `--worktree DIR` / `--all` | Same, scoped wider; docker entries go through the docker runner. Asks for confirmation unless `--yes`. |
| `devrun gc [--kill-orphans] [--yes] [--quiet] [--json]` | Remove registry entries whose PID is gone. Report **orphans**: a listening port in the devrun range with no registry entry, or a container whose compose project starts with `devrun-` but isn't in the registry. `--kill-orphans` stops them (listeners owned by you; `docker compose -p NAME down` for compose projects, never volumes or non-`devrun-` projects), confirming first unless `--yes`. |
| `devrun notify [name] "message" [--sound none|done|request] [--json]` | Flag this server as needing human inspection: `herdr notification show` with URL, project, worktree and message. Returns immediately. Records `flagged` in the registry so `ls`/`watch` highlight it. |
| `devrun clear [name]` | Remove the flag after you have inspected it. |
| `devrun hook install [--force]` / `devrun hook wrapper` | Install a git `post-checkout`/worktree-removal hook (and print the `git worktree remove` wrapper to use) that runs `devrun stop` for a worktree that is being deleted. Git has no native hook for worktree removal, so v1 also checks in `gc` and on every `ls` for entries whose worktree directory no longer exists and stops those. |
| `devrun doctor [--json]` | Check herdr is on PATH, the state dir is writable, native listener tools are available and the port range is free. |

## Data model

Registry: `~/.local/state/devrun/registry.json`, guarded by an `flock` lockfile. Written atomically (tmp file plus rename).

```json
{
  "key": "<project>:<worktree-path>:<name>",
  "project": "my-app",
  "worktree": "/home/user/code/my-app-feature",
  "branch": "feat/card-art",
  "name": "web",
  "kind": "proc",
  "pid": 12345,
  "pgid": 12345,
  "cmd": ["npm", "run", "dev"],
  "ports": { "PORT": 41237 },
  "compose_project": null,
  "started_at": "...",
  "owner": { "herdr_pane": "...", "herdr_workspace": "...", "agent": "claude" },
  "flagged": { "at": "...", "message": "check the new deck layout" },
  "log": "..."
}
```

## Port allocation

- Range **41000-41999** **[assumed]**, chosen to stay clear of the usual dev-server ports (3000, 4200, 5173, 8000-8999).
- Stable: port = lowest free port from a hash of the key, then probed. The mapping is persisted, so the same worktree gets the same port every run.
- "Free" means not in the registry **and** not listening (Linux `/proc/net/tcp{,6}` with `ss` fallback; macOS `lsof` plus IPv4/IPv6 bind probes), so processes not started by devrun are respected.
- The registry lock covers allocate-and-start, so two agents starting at once can't get the same port.

## Docker / compose mode

The agent allocates the port, the compose file consumes it, and devrun owns the lifecycle:

```yaml
# docker-compose.yml in the project
services:
  web:
    ports: ["${WEB_PORT:-3000}:3000"]
```

`devrun up --compose --ports web` then runs, in the worktree: `WEB_PORT=41237 COMPOSE_PROJECT_NAME=devrun-my-app-ab12cd docker compose up -d --build --wait`.

- **Isolation:** the unique compose project name keeps container, network and volume names per worktree, so four worktrees of one repo don't collide. Compose files with a hardcoded `container_name:` will still collide. `doctor` and `up` warn about this.
- **Handle:** the compose project name, not a PID. Liveness comes from `docker compose -p <name> ps --format json`, and `up` means every service is running or healthy and the allocated port accepts connections.
- **Multiple ports:** `--ports web,api,db` allocates one port each and exports `WEB_PORT`, `API_PORT`, `DB_PORT`. `--port-var web=FRONTEND_PORT` overrides a variable name.
- **Port allocation** also counts ports published by any running container (`docker ps`), not just host listeners.
- **Orphans:** `gc` lists `devrun-*` compose projects with no registry entry. `--kill-orphans` runs `down` on them.
- **Projects that can't read env vars** (hardcoded ports in compose): `--override` generates a temporary `docker-compose.devrun.yml` that remaps published ports. v1 documents this and defers the implementation.
- **Safety:** devrun never touches containers outside its `devrun-` prefix, never mounts the Docker socket anywhere, and never runs `docker system prune`.
- **Logs:** `devrun logs` maps to `docker compose logs`.

## Process management

- Track process identity with kernel start time (clock ticks on Linux, microsecond timestamps on macOS) so recycled PIDs are never treated as the original process.
- Start with `setsid` so the whole tree (npm, then vite, then node) shares a process group. Stop kills the group.
- Liveness: PID alive **and** its process group is listening on the recorded port. If the PID is alive but the port isn't listening, state is `starting` for the first N seconds and `hung` after that.
- Detect port drift: if the framework ignored `PORT` and bound elsewhere, find the actual listening port of the group via `/proc` socket ownership on Linux or `lsof` on macOS and report a warning in `up`.
- Servers survive the agent's session ending. `gc` and `stop` are the cleanup paths. Optional `--ttl 4h` auto-expiry is deferred.

## herdr integration

- Owner detection: read herdr's pane and workspace from the environment, or from `herdr pane current` when env vars are absent. Needs a check of what herdr actually exports.
- `notify` uses `herdr notification show`. Fallback if herdr isn't running: print to stderr and write the flag only.
- `devrun watch` is meant to be launched in its own pane: `herdr pane split ... -- devrun watch`.
- Open question: whether `herdr api snapshot` can be used to map a PID to a pane so `owner` fills in automatically.

## Agent integration

- `AGENTS.md` / CLAUDE.md snippet shipped in the repo:
  - Never hardcode a port. Start servers with `devrun up -- <cmd>`, or `devrun up --compose --ports web` for docker projects (the compose file must use `${WEB_PORT}`).
  - Get the URL with `devrun url` and give it to Playwright MCP.
  - When something needs human eyes: `devrun notify "what to look at"`, then continue working.
  - Before finishing a task, run `devrun stop` unless told to leave it up.
- Optional skill form of the same text, in `.claude/skills/devrun/`.

## Requirements

**Functional:** everything in Commands; idempotent `up`; safe under concurrent invocation; `--json` on every read command with stable field names.

**Non-functional:**
- Single static Go binary, no runtime dependencies. Go 1.19 is installed, so no generics-heavy or newer-stdlib code.
- Only external tools: `git`, and optionally `herdr` and `docker` (with the compose plugin). On Linux, listening ports come from direct `/proc/net/tcp` parsing; `ss` is a fallback. macOS uses built-in `lsof` for listener ownership and probes candidate ports with IPv4/IPv6 binds on wildcard and local addresses during allocation; process identity uses kernel `sysctl` start timestamps. Ownership detection and orphan cleanup are limited to processes visible to the invoking user.
- Registry operations under 50ms. `ls` must not block on dead processes.
- Never kills a PID it didn't start. `stop` verifies the PID's start time against the registry to avoid PID-reuse kills.
- Bind-address note: `devrun` doesn't change what interface the app listens on. Document that `127.0.0.1` is the safe default.

## Milestones

1. **M1 core** (done): registry and lock, port allocation, `up`/`ls`/`stop`/`url`/`logs`, unit tests for allocation and concurrency.
2. **M2 docker** (done): `up --compose`, multi-port env vars, compose-project handle, liveness via `compose ps`, `down` on stop, container-published ports counted in allocation.
3. **M3 hygiene** (done): `gc`, orphan detection (processes and compose projects), port-drift detection, PID-reuse safety, `stop --project/--all`, stale-worktree auto-stop, `hook install`.
4. **M4 herdr** (done): owner detection, `notify`/`clear`, `watch` TUI, `doctor`.
5. **M5 agents** (done): README, `docs/agent-guide.md` snippet, `skills/devrun/SKILL.md`, two-worktree end-to-end test (Playwright optional).
6. **M6 (later)**: `--override` port remapping for compose files with hardcoded ports, TTL expiry, MCP wrapper.

## Open questions

1. What does herdr export into a pane's environment? I need to read `herdr.dev/llms.txt` before M4.
2. Which of your projects use compose, and do they hardcode ports or `container_name:`? That decides whether `--override` has to move up from M6.
3. Should `stop` on a docker stack drop volumes by default? I assumed no, because databases are expensive to rebuild.
