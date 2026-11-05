# Architecture

The app is a client of the T3 Code server. It never runs agents itself: it finds (or starts) a
server, authenticates, subscribes to state, renders it, and sends commands back.

```
 ┌──────────────── t3-desktop (GPUI, main thread) ────────────────┐
 │  Workspace view ── reads ──▶ SyncState (per environment)        │
 │        │                         ▲                              │
 │   commands                  watch<u64> revision                 │
 └────────┼─────────────────────────┼──────────────────────────────┘
          ▼                         │
 ┌──────── t3-client (tokio) ───────┴──────────────────────────────┐
 │  Connection actor ◀── Effect RPC over /ws ──▶ t3 server (Node)  │
 │  Sync engine: shell + open thread subscriptions, reconnect      │
 └─────────────────────────────────────────────────────────────────┘
          ▲
 ┌──────── t3-backend ───────────────────────────────────────────────┐
 │  Supervisor: spawn `node <entry> --bootstrap-fd 0`, readiness,    │
 │  restart with backoff, graceful stop. Connectors: local / remote. │
 └───────────────────────────────────────────────────────────────────┘
```

## Crates

| Crate | Depends on | What it holds |
| --- | --- | --- |
| `t3-protocol` | | Effect RPC frames (`Request`, `Chunk`, `Exit`, `Ack`, `Interrupt`, `Ping`/`Pong`), exits and causes, and `RpcSession`, a socket-free state machine for request ids, acks and stream lifecycles |
| `t3-contracts` | | Typed server API: commands, domain events, shell and thread projections, config. Unknown fields are kept in `extra` maps so newer servers round-trip |
| `t3-state` | contracts | Pure reducers: `ShellState` (projects and threads list), thread projection `apply_event`, and transcript derivation for the view |
| `t3-client` | protocol, contracts, state | Pairing URLs, the HTTP auth chain, the WebSocket `Connection` actor, typed API calls, and the sync engine |
| `t3-backend` | client | Bootstrap tokens, launch resolution, the process supervisor, and `LocalConnector` / `RemoteConnector` |
| `t3-desktop` | all | CLI, the GPUI-free editing model and view models, and the GPUI views |

Only `t3-desktop::ui` touches GPUI. Everything else is tested as plain Rust.

## Attaching to a running server

T3 Code's server writes `<t3Home>/userdata/server-runtime.json` (`dev/` for development builds)
with its pid, port and origin, and holds a lock on `<t3Home>` while it runs. At startup, unless
`--launch-server` is given, the app reads that file, checks the pid is alive and the descriptor
answers, and attaches (`AttachedConnector`):

1. Use the saved session for that server if it has not expired.
2. Otherwise mint a one-time pairing token with the server's own CLI,
   `t3 pair --base-dir <t3Home> --label "T3 Code (Rust)" --ttl 5m`. The CLI must be the server's
   exact version, since it opens the server's database; the app tries `<t3Home>/bin/t3`, then
   `t3` on `PATH`, then `npx --yes t3@<version>`, skipping any whose `--version` differs.
3. Exchange the token at `/oauth/token` for a bearer and save it (atomically, mode 0600).
4. Continue with the ticket and upgrade below. A refused bearer drops the saved session, so the
   next connect pairs again.

Every reconnect rediscovers the server, so a restarted T3 Code (new port) is picked up.

## Starting a local server

When no server is running, the app starts one:

1. Resolve how to launch: `--server-entry` with `node`, else `t3` on `PATH`, else
   `npx --yes t3@0.1.7`.
2. Spawn `<launch> --bootstrap-fd 0` with stdin piped and write one JSON line (the bootstrap
   envelope: mode `desktop`, loopback host and port, `t3Home`, the bootstrap secret). The server
   waits about a second for it.
3. Poll `GET /.well-known/t3/environment` until it answers 2xx.
4. If the process exits, restart after `min(500ms × 2^attempt, 10s)`; the attempt count resets
   once a run becomes ready.
5. On shutdown send SIGTERM, wait out the grace period, then kill. The app also stops servers on
   SIGINT, SIGTERM and SIGHUP, and on a panic on the UI thread, so no server is left orphaned.
6. Exit code 78 means another server owns `<t3Home>`; the supervisor reports it and stops instead
   of restarting.

## Authentication

```
GET  /.well-known/t3/environment          descriptor; protocol version must be 2
POST /oauth/token                         bootstrap credential → bearer token
POST /api/auth/websocket-ticket/rpc       bearer → one-time ticket (bare path on 404)
GET  /ws?wsTicket=…&orchestrationProtocol=2&clientSurface=…   upgrade
```

The local bootstrap credential is `HMAC-SHA256(secret, "t3-desktop-bootstrap:{window}")` over
12-hour windows; the server accepts the previous, current and next window. An attached server's
credential comes from `t3 pair`, and a remote environment's from a pairing URL. Bearer tokens are
cached in memory (and on disk for attached servers) and dropped when a ticket or upgrade is
refused.

## Transport

`Connection` owns the socket on a tokio task. Requests and streams are futures and `Stream`s;
dropping a stream sends an `Interrupt`. Every chunk is acked once delivered. The client pings
every 5 seconds and drops a socket that has been silent for 15.

## Sync

`t3_client::sync::spawn` runs one engine per environment. It connects through a `Connector`,
loads the server config, subscribes to the shell stream and, when a thread is open, that thread's
stream. Each subscription asks for a completion marker and resumes from the last applied
sequence after a reconnect; a gap makes it resubscribe from a fresh snapshot. Reconnects back off
with jitter: the ceiling is `min(300s, 1s × 2^(n+1))` and the delay is half the ceiling plus a
random half.

State lives behind a lock; every change bumps a `watch<u64>` revision. The UI awaits revisions on
GPUI tasks (tokio channels are runtime agnostic), coalesces them over 16 ms, and re-renders.

## The view

`t3-desktop` turns state into plain view data before drawing, so the rules are unit tested:

- `sidebar` ports upstream's sidebar logic: snoozed beats settled beats pinned beats working;
  subagents and side chats are hidden; each shelf has its own order; cards get a project monogram
  and color, a compact time (`15h`) or a working timer (`Working 8m`).
- `timeline` groups consecutive tool work into one summary row ("Used Playwright integration,
  used 1 tool, and ran 1 command") and folds a finished run's work before its final answer into
  "Worked for 2m 5s" (or "You stopped after …").
- `markdown` parses assistant text with pulldown-cmark into blocks of styled runs.

The chat is a GPUI `list` over those rows; it re-measures only from the first row whose content
changed, and expanding a row re-measures just that row.

## Threads and runtimes

GPUI owns the main thread. A two-worker tokio runtime runs networking and the server process.
GPUI tasks await tokio futures directly; nothing blocks the main thread except the bounded
shutdown on quit.
