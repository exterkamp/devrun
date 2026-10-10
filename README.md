# devrun

A small CLI that lets parallel coding agents, each on its own git worktree, start dev servers without port collisions. Every server gets a stable port, is recorded with its project, worktree, branch and owner, and can be listed, flagged for a human, and cleaned up. Supports Linux and macOS (Apple Silicon and Intel). See `SPEC.md` for the design.

## Install

```bash
go install github.com/exterkamp/devrun/cmd/devrun@latest
```

Or from a checkout:

```bash
mkdir -p ~/.local/bin
go build -o ~/.local/bin/devrun ./cmd/devrun
```

Make sure `~/.local/bin` is on your `PATH`.

Needs Go 1.19+ and `git`. `herdr` (notifications, owner detection) and `docker` with the compose plugin (compose mode) are optional. Check the setup with `devrun doctor`.

### macOS setup

Install Go and Git with Homebrew, then build from this checkout:

```bash
brew install go git
mkdir -p ~/.local/bin
go build -o ~/.local/bin/devrun ./cmd/devrun
export PATH="$HOME/.local/bin:$PATH"
devrun doctor
```

Add the `export PATH` line to `~/.zshrc` to keep it in new terminals. If using
`go install` instead, add `$(go env GOPATH)/bin` to your `PATH`.

macOS uses the built-in `/usr/sbin/lsof`; no Linux
`ss` utility or `/proc` mount is needed. Process identity is read from the
kernel with microsecond-resolution start timestamps. Listener ownership for
port drift and orphan cleanup is limited to processes visible to your user;
allocation also probes candidate ports with IPv4 and IPv6 binds on wildcard and local addresses, so
sockets owned by other users are skipped even when their ownership is hidden. Run devrun as your normal user.

For `--compose`, install and start a Docker runtime that supplies
`docker compose` (for example Docker Desktop). Ordinary process mode needs no
Docker runtime. Herdr is optional on both platforms.

## Quick start

Process mode, from inside a git worktree:

```bash
devrun up -- npm run dev                                   # PORT is set for you
devrun up -- python3 -m http.server {port} --bind 127.0.0.1  # or use {port} in the args
devrun url                                                 # http://127.0.0.1:41237
devrun notify "check the new card layout"                  # flag it for a human, returns at once
devrun clear                                               # after it was inspected
devrun stop
```

`up` is idempotent (a second call prints the same URL), waits until the port accepts connections, and fails if the process dies or never listens. Use `--name` to run several servers in one worktree, then pass the name to `url`, `logs`, `stop`.

Compose mode: the compose file publishes ports through variables and devrun allocates them.

```yaml
services:
  web:
    ports: ["${WEB_PORT:-3000}:3000"]
```

```bash
devrun up --compose --ports web            # exports WEB_PORT, runs docker compose up -d --wait
devrun up --compose --ports web,api --build
devrun stop                                # docker compose down; volumes are kept
```

Avoid `container_name:` in compose files; it collides across worktrees.

## Command reference

`devrun <command> --help` prints the flags of each command. All read commands accept `--json`.

| Command | Purpose |
|---|---|
| `up [--name web] [--port-env PORT] [--wait 60s] -- <cmd...>` | Start a server with an allocated port. `{port}` in args is substituted. |
| `up --compose [--ports web,api] [--port-var web=VAR] [-f file] [--build] [-- <extra compose args>]` | Start a compose stack. One port per name in `--ports` (default a single `PORT`), exported as `WEB_PORT`, `API_PORT`, or the name given by `--port-var`. `-f` is repeatable. |
| `ls [--project P] [--json] [--all]` | List servers: project, worktree/branch, name, port, URL, PID, kind, state, age, owner. Dead entries need `--all`. |
| `url [target] [--json]` | Print a server's URL. |
| `logs [target] [-f] [-n lines]` | Show the server log (`docker compose logs` in compose mode). |
| `stop [target]` | Stop one server. |
| `stop --project P` / `--worktree DIR` / `--all` [`--yes`] | Stop wider scopes; asks to confirm unless `--yes`. |
| `notify [name] "message" [--sound none\|done\|request] [--json]` | Flag a server for human inspection through `herdr notification show`; returns immediately. |
| `clear [target]` | Remove the flag. |

`target` (for `stop`, `logs`, `url`, `clear`, `notify`) resolves in order: a name in this worktree, an exact port (`devrun stop 41237`), `project/name`, then a name across all servers (the global fallback), so it works from any directory. If a target matches several servers, or nothing is given outside a git repo with several servers, devrun lists the candidates and exits non-zero without acting; retry with the port or `project/name`.
| `watch [--project P] [--interval 1s] [--once]` | Live view for a herdr pane; flagged rows are highlighted. |
| `gc [--kill-orphans] [--yes] [--quiet] [--json]` | Drop dead entries, stop servers of removed worktrees, report orphans (listeners in the range with no entry, `devrun-*` compose projects not in the registry). `--kill-orphans` stops them. |
| `hook install [--force]` / `hook wrapper` | Install a git `post-checkout` hook that runs `gc`; print a `gwt-rm` shell function that stops servers before `git worktree remove`. |
| `doctor [--json]` | Check herdr, the state dir, native listener tools and the port range. |

## Ports

Only **41000-41999** is used. A worktree's port comes from a hash of its key (project, worktree path, name) and is probed upward. It is persisted, so the same worktree gets the same port after `stop` and `up`. A port is free if it is not in the registry, not listening on the host, and (compose mode) not published by a running container. Ports outside that range, such as 3000 or 8080, are never allocated or touched.

## Where things live

State directory: `~/.local/state/devrun/` (override with `$DEVRUN_STATE_DIR`, else `$XDG_STATE_HOME/devrun`).

- `registry.json`: all entries, guarded by an `flock` lockfile, written atomically.
- `logs/`: one log file per server.

## Safety guarantees

- Never kills a PID it did not start: `stop` checks the process start time against the registry, so PID reuse is safe. Process mode signals the whole process group (SIGTERM, then SIGKILL after 5s).
- Compose mode touches only projects named `devrun-<project>-<hash>`, never other containers, and never runs `docker system prune`, mounts the Docker socket, or removes volumes.
- Ports outside 41000-41999 are never allocated; ports in use by other processes are skipped.
- Allocation and start happen under the registry lock, so simultaneous `up` calls cannot get the same port.
- devrun does not change what interface your app binds; use `127.0.0.1`.
- `gc --kill-orphans` and wide `stop` ask for confirmation unless `--yes`.

## For agents

See [docs/agent-guide.md](docs/agent-guide.md) for a copy-paste `CLAUDE.md`/`AGENTS.md` snippet and `skills/devrun/SKILL.md` for the Claude Code skill (copy it to `~/.claude/skills/devrun/`).

## Development

```bash
go vet ./... && go test -count=1 ./...
```

CI runs the suite on Linux and macOS (Apple Silicon and Intel). The end-to-end tests build the binary and use `python3`, `git` and a fake `herdr`; they skip with a message if a tool is missing.
