---
name: dev-environment
description: >
  Run, debug, or extend the local Docker Compose development stack — the
  gateway-service to echo-service call path, Postgres and Redis, the container
  healthchecks, and the DevStart/DevStop/DevDelete/DevRemove .cmd scripts. Use
  when adding a service to compose, when a container will not become healthy,
  when a port is already in use, or when someone asks how to start or reset the
  local environment.
---

# Local development stack

Thirteen containers: six services, NATS, Postgres, Redis, Jaeger, Prometheus, Grafana.

| What | Where |
| --- | --- |
| Gateway | http://localhost:8080 |
| Grafana dashboard | http://localhost:3000 (no login) |
| Jaeger traces | http://localhost:16686 |
| Prometheus | http://localhost:9090 |
| NATS monitoring | http://localhost:8222/jsz?streams=1 |

## The three observability views

They answer different questions, and reaching for the wrong one wastes time:

- **Metrics** (Grafana) — how much, how often, is it getting worse. Start here.
- **Traces** (Jaeger) — what did *this one request* cause, across every service.
- **Logs** (`DevLogs.cmd`) — exactly what happened, one event at a time.

A metric tells you orders got slower at 14:20. A trace tells you why that one
was slow.

## The scripts

Run these from anywhere; each one `pushd`s to the repo root first.

| Script | What it does | What survives |
| --- | --- | --- |
| `DevStart.cmd` | Build and start, wait for healthy, print URLs | — |
| `DevStatus.cmd` | What is running and whether it is healthy | — |
| `DevLogs.cmd [service]` | Follow logs; Ctrl-C stops following, not the stack | — |
| `DevStop.cmd` | Stop containers | Containers, images, volumes |
| `DevDelete.cmd` | Remove containers and network | Images, volumes (data kept) |
| `DevRemove.cmd [-y]` | Remove containers, network, built images, volumes | Nothing |
| `DevDemo.cmd` | Run and narrate the whole demonstration | — |
| `DevReplay.cmd [--dry-run]` | Put dead letters back on the queue | — |

`DevRemove.cmd` prompts for confirmation because it deletes the Postgres and
Redis volumes. Pass `-y` to skip the prompt in a script.

## Non-negotiable rules

-1. **A new service must expose `/metrics`, and there are two ways to get it.**
   Services that call `health::serve` (the consumers) get it automatically.
   Services that build their own router — `gateway-service`, `echo-service` —
   must `.merge(service_core::metrics_routes())` themselves. Forgetting is
   silent: the service runs perfectly, passes every healthcheck, and simply
   never appears in Prometheus. Check with
   `curl http://localhost:9090/api/v1/targets?state=active` after adding one.

0. **`.sql` files are pinned to LF in `.gitattributes`.** sqlx checksums each
   migration's bytes and refuses to run when a previously applied file has
   changed. Letting `* text=auto` rewrite them to CRLF on a Windows checkout
   changes those bytes, and every database-using service then dies at startup
   with *"migration 1 was previously applied but has been modified"*. It looks
   like database corruption; it is a line ending.

1. **Healthchecks call the service binary, not curl.** The runtime image is
   distroless: no shell, no curl, no wget. Each service answers
   `service healthcheck` by probing its own `/healthz` over a TCP socket
   (`service_core::health::self_check`). A `CMD-SHELL` healthcheck, or one
   invoking curl, fails instantly with "executable file not found".

2. **`self_check` must not half-close the socket.** Calling
   `shutdown(Shutdown::Write)` after sending the request reads as a disconnect
   to hyper, which drops the connection without replying, and every healthcheck
   fails. `Connection: close` in the request header does the job instead.

3. **`depends_on` needs `condition: service_healthy`.** Plain `depends_on` waits
   only for the container to exist, not to be usable — the gateway would start
   and fail its first requests.

4. **Ports come from compose, not from assumptions.** `DevStart.cmd` asks
   `docker compose port` which host port was actually published, because a
   `.env` file can change them. Do not hardcode 8080 in new scripts.

5. **`DATABASE_URL` and `REDIS_URL` are not passed to the services.** Postgres
   and Redis run so the connection details exist before the first crate needs
   them. A service that does not use a database should not be handed one; add
   the variable to that service in `compose.yaml` when its code actually reads
   it.

## Adding a service to the stack

1. Create the crate under `Microservices/` (the workspace globs `Microservices/*`).
2. Add a block to `compose.yaml` copying `echo-service`: change `SERVICE`, the
   image name, and the host port.
3. Give it the same healthcheck line verbatim — the binary path
   `/usr/local/bin/service` is the same in every image, because the Dockerfile
   renames the binary on the way in.
4. Nothing else changes. The CI gate and the image workflow both discover
   crates from `cargo metadata`.

## Configuration

`compose.yaml` has a default for every variable, so no `.env` file is required.
Create one to override:

```
GATEWAY_PORT=9000
ECHO_PORT=9001
POSTGRES_PASSWORD=something-else
RUST_LOG=debug
```

`.env` is gitignored — it is local-only, and it is where a password would go.

## Debugging

- **A container never becomes healthy**: `DevLogs.cmd <service>`. If the logs
  look fine, the healthcheck itself is failing — check rules 1 and 2 above.
- **"port is already allocated"**: something else owns 8080. Set `GATEWAY_PORT`
  in `.env`.
- **`/relay` returns 502**: the gateway could not reach echo-service. Check
  `ECHO_SERVICE_URL` matches the compose service name, and that echo-service is
  healthy — compose DNS resolves service names, not container names.
- **You cannot `docker exec` a shell**: distroless has none. Rebuild with
  `docker build --target builder` and exec into that image instead.
- **Changed Rust code but the container runs the old build**: `DevStart.cmd`
  passes `--build`, but a stale image can survive. `DevDelete.cmd` then
  `DevStart.cmd` forces a clean container.

## Known gaps

1. No hot reload. A code change means a rebuild — roughly 20 seconds warm,
   since the cargo-chef dependency layer is cached.
2. Postgres and Redis have no schema or migrations, because nothing uses them.
3. The stack is not wired to the published GHCR images; compose always builds
   locally. That is deliberate for development.
