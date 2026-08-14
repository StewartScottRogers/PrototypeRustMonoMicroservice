---
name: dev-environment
description: >
  Run, debug, or extend the local Docker Compose development stack — the call
  path from gateway-service to echo-service, PostgreSQL and Redis, the
  container health checks, and the DevStart, DevStop, DevDelete and DevRemove
  scripts. Use when adding a service to compose, when a container will not
  become healthy, when a port is already in use, or when someone asks how to
  start or reset the local environment.
---

# Local development stack

Thirteen containers: six services, NATS, PostgreSQL, Redis, Jaeger, Prometheus
and Grafana.

> **Write everything out in full.** No acronyms or abbreviations in prose. A
> widely recognised acronym may follow the full term in parentheses on first
> mention only. Identifiers — service names, environment variables, ports —
> stay as they are.

| What | Where |
| --- | --- |
| **Console — everything in one page** | **http://localhost:8090** (`DevConsole.cmd`) |
| Gateway | http://localhost:8080 |
| Mimic panel alone | http://localhost:8090/mimic |
| Grafana dashboard | http://localhost:3000 (no login) |
| Jaeger traces | http://localhost:16686 |
| Prometheus | http://localhost:9090 |
| NATS monitoring | http://localhost:8222/jsz?streams=1 |

## The three observability views

They answer different questions, and reaching for the wrong one wastes time:

- **Metrics** (Grafana) — how much, how often, is it getting worse. Start here.
- **Traces** (Jaeger) — what did *this one request* cause, across every
  service.
- **Logs** (`DevLogs.cmd`) — exactly what happened, one event at a time.

A metric tells you orders got slower at 14:20. A trace tells you why that one
was slow.

## The scripts

Run these from anywhere; each one runs `pushd` to the repository root first.

| Script | What it does | What survives |
| --- | --- | --- |
| `DevStart.cmd` | Build and start, wait for healthy, print addresses | — |
| `DevStatus.cmd` | What is running and whether it is healthy | — |
| `DevLogs.cmd [service]` | Follow logs; Ctrl-C stops following, not the stack | — |
| `DevStop.cmd` | Stop containers | Containers, images, volumes |
| `DevDelete.cmd` | Remove containers and network | Images, volumes (data kept) |
| `DevRemove.cmd [-y]` | Remove containers, network, built images, volumes | Nothing |
| `DevDemo.cmd` | Run and narrate the whole demonstration | — |
| `DevReplay.cmd [--dry-run]` | Put dead letters back on the queue | — |
| `DevConsole.cmd` | Open every graphical interface in one framed page; starts the stack if needed | — |

**Grafana must be told it may be embedded.** It sends `X-Frame-Options: deny`
by default, so the Grafana tab in the console comes up blank with nothing on
screen to explain why. `GF_SECURITY_ALLOW_EMBEDDING: "true"` in `compose.yaml`
is what makes it work — correct for a local demonstration, and a clickjacking
hole on anything reachable from outside.

The addresses behind the external tabs of the console come from `/api/links`,
populated from `GRAFANA_URL`, `JAEGER_URL` and `PROMETHEUS_UI_URL` in compose.
Those are *published host ports* — the page renders in your browser, so a
container name would not resolve, and hard-coding 3000 breaks the moment
someone sets `GRAFANA_PORT`.

`DevRemove.cmd` prompts for confirmation because it deletes the PostgreSQL and
Redis volumes. Pass `-y` to skip the prompt in a script.

## Non-negotiable rules

-1. **A new service must expose `/metrics`, and there are two ways to get it.**
   Services that call `health::serve` (the consumers) get it automatically.
   Services that build their own router — `gateway-service`, `echo-service` —
   must call `.merge(service_core::metrics_routes())` themselves. Forgetting
   is silent: the service runs perfectly, passes every health check, and
   simply never appears in Prometheus. Check with
   `curl http://localhost:9090/api/v1/targets?state=active` after adding one.

0. **Files ending in `.sql` are pinned to line-feed endings in
   `.gitattributes`.** sqlx takes a checksum of the bytes of each migration
   and refuses to run when a previously applied file has changed. Letting
   `* text=auto` rewrite them to carriage-return-plus-line-feed on a Windows
   checkout changes those bytes, and every database-using service then dies at
   startup with *"migration 1 was previously applied but has been modified"*.
   It looks like database corruption; it is a line ending.

1. **Health checks call the service binary, not curl.** The runtime image is
   distroless: no shell, no curl, no wget. Each service answers
   `service healthcheck` by probing its own `/healthz` over a raw socket
   (`service_core::health::self_check`). A `CMD-SHELL` health check, or one
   invoking curl, fails instantly with "executable file not found".

2. **`self_check` must not half-close the socket.** Calling
   `shutdown(Shutdown::Write)` after sending the request reads as a
   disconnect to hyper, which drops the connection without replying, and every
   health check fails. `Connection: close` in the request header does the job
   instead.

3. **`depends_on` needs `condition: service_healthy`.** Plain `depends_on`
   waits only for the container to exist, not to be usable — the gateway would
   start and fail its first requests.

4. **Ports come from compose, not from assumptions.** `DevStart.cmd` asks
   `docker compose port` which host port was actually published, because a
   `.env` file can change them. Do not hard-code 8080 in new scripts.

5. **`DATABASE_URL` and `REDIS_URL` are not passed to every service.**
   PostgreSQL and Redis run so the connection details exist before the first
   crate needs them. A service that does not use a database should not be
   handed one; add the variable to that service in `compose.yaml` when its
   code actually reads it.

## Adding a service to the stack

1. Create the crate under `Microservices/` (the workspace globs
   `Microservices/*`).
2. Add a block to `compose.yaml` copying `echo-service`: change `SERVICE`, the
   image name, and the host port.
3. Give it the same health check line verbatim — the binary path
   `/usr/local/bin/service` is the same in every image, because the Dockerfile
   renames the binary on the way in.
4. Nothing else changes. The continuous integration gate and the image
   workflow both discover crates from `cargo metadata`.

## Configuration

`compose.yaml` has a default for every variable, so no `.env` file is
required. Create one to override:

```
GATEWAY_PORT=9000
ECHO_PORT=9001
POSTGRES_PASSWORD=something-else
RUST_LOG=debug
```

`.env` is ignored by git — it is local only, and it is where a password would
go.

## Debugging

- **A container never becomes healthy**: `DevLogs.cmd <service>`. If the logs
  look fine, the health check itself is failing — check rules 1 and 2 above.
- **"port is already allocated"**: something else owns 8080. Set
  `GATEWAY_PORT` in `.env`.
- **`/relay` returns 502**: the gateway could not reach echo-service. Check
  that `ECHO_SERVICE_URL` matches the compose service name, and that
  echo-service is healthy — name resolution inside compose resolves service
  names, not container names.
- **You cannot start a shell with `docker exec`**: a distroless image has
  none. Rebuild with `docker build --target builder` and enter that image
  instead.
- **Changed Rust code but the container runs the old build**: `DevStart.cmd`
  passes `--build`, but a stale image can survive. Run `DevDelete.cmd` and
  then `DevStart.cmd` to force a clean container.

## Known gaps

1. No hot reload. A code change means a rebuild — roughly 20 seconds warm,
   since the cargo-chef dependency layer is cached.
2. Redis is started but nothing reads or writes it yet; PostgreSQL carries the
   real schema, owned by `db-core`.
3. The stack is not wired to the images published to the GitHub Container
   Registry; compose always builds locally. That is deliberate for
   development.
