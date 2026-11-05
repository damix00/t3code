# T3 Code desktop, native (Rust + GPUI)

A native desktop client for T3 Code built on [GPUI](https://www.gpui.rs), with no webview and no
Electron. It talks to the same Node server (`apps/server`) as the Electron app, over the same
Effect RPC protocol.

T3 Code is a GUI for coding agents (Codex, Claude, and others). Upstream, the desktop app is
Electron wrapping the web UI, and it talks to a Node server (`t3`) that runs the agents. This
project replaces the Electron shell and the web UI with a native GPUI app. The Node server is
unchanged: the app connects to the one T3 Code is already running, starts its own the way the
Electron app does, or connects to a remote one.

## Status

This is an early port, written against `t3@0.1.7`:

- Attaches to the server T3 Code is already running for `~/.t3`, so you see the same projects and
  threads (see [Connecting to your server](#connecting-to-your-server))
- Otherwise starts and supervises a local server (bootstrap over fd 0, readiness probe, restart
  with backoff, graceful stop, no orphans on SIGINT/SIGTERM/SIGHUP or a UI-thread panic)
- Authenticates with the rotating desktop bootstrap credential, then a WebSocket ticket
- Speaks the server's Effect RPC protocol over `/ws`, with keepalive, acks and stream interrupts
- Live shell sync (projects, threads, status) with resume-from-sequence and reconnect backoff
- T3 Code's dark look: the thread inbox with Working, Snoozed and Settled shelves, project
  monograms, live working timers, breadcrumb header, composer card
- Transcript with markdown, tool work summarized per group ("Ran 3 commands"), finished runs folded
  into "Worked for 2m 5s", plans and todos, approvals (allow / allow for session / deny), stop
- Thread search, back and forward, add project, new thread
- Multi-line composer with IME, word movement and selection (no undo yet)
- Pairing with remote environments (pairing URLs and `t3code://pair` links via `--remote`)

Not ported yet: the diff panel and changed-files card, terminals, the git/branch toolbar, the
command palette, attachments, `@` file and `/` command completion, model and mode pickers (new
threads use the server's default model), pinning, snoozing and settling from the sidebar,
settings, the light theme, notifications and auto-update. Inline code in messages uses the body
text size, since GPUI cannot vary font size within a paragraph.

Verified live against a real T3 Code 0.1.7 server: attaching and reconnecting from a saved
session, launching a server, auth, sync, project creation, shutdown, and a thread streaming a live
agent run.

Remote environments' bearer tokens are held in memory only, so a remote environment is re-paired
on each launch.

## Getting started

You need Rust (the toolchain is pinned in `rust-toolchain.toml`; `rustup` picks it up) and
Node.js 22+ for the server.

```sh
cargo run -p t3-desktop
```

### Connecting to your server

If T3 Code is running, the app finds its server through `~/.t3/userdata/server-runtime.json` and
connects to it rather than starting another one (a second server could not use the same `~/.t3`
anyway: the first holds a lock on it). The first time, it pairs through T3 Code's own CLI
(`~/.t3/bin/t3 pair`, or a `t3` on `PATH` of the same version), so a client named
"T3 Code (Rust)" appears under Settings → Connections in T3 Code; you can revoke it there. The
session is saved, readable only by you, in `t3-code-rust/sessions.json` under your user data
directory (`~/Library/Application Support` on macOS), and later launches reuse it until it
expires.

### Starting a server

With no server running, or with `--launch-server`, the app starts one and looks for it in this
order:

1. `--server-entry <path>` (or `T3CODE_SERVER_ENTRY`): a server entry script, run with `node`
2. `t3` on `PATH` (`npm i -g t3`)
3. `npx --yes t3@0.1.7`

On Linux, install GPUI's system libraries first: `./scripts/install-linux-deps.sh`.

### Options

| Flag | Env | Meaning |
| --- | --- | --- |
| `--server-entry <PATH>` | `T3CODE_SERVER_ENTRY` | Run this server entry with `node` |
| `--t3-home <DIR>` | `T3CODE_HOME` | Server state directory (default `~/.t3`) |
| `--port <PORT>` | | Loopback port for a server the app starts (default: a free one) |
| `--launch-server` | | Start a server even if one is running (use with a separate `--t3-home`) |
| `--remote <URL>` | | Pair with a remote environment: a pairing URL or `t3code://pair` link |
| `--thread <ID>` | | Open this thread once connected |
| `--no-local` | | Do not start a local server (requires `--remote`) |
| `--log <FILTER>` | `T3CODE_LOG` | Log filter, e.g. `debug` or `t3_client=trace` |

Logs go to stderr.

## How it fits together

```
crates/
  t3-protocol   Effect RPC wire format: frames, exits, causes, the client session state machine
  t3-contracts  Server API types: commands, events, shell and thread projections
  t3-state      Pure reducers: shell state, thread state, transcript derivation
  t3-client     Pairing, auth, the WebSocket transport, typed API, the sync engine
  t3-backend    Local server launch, supervision, bootstrap tokens, connectors
  t3-desktop    The GPUI app: text editing model, view models, UI
```

Everything below `t3-desktop`'s `ui` module is plain Rust with no GPUI dependency, and is where
most of the tests live. See [docs/architecture.md](docs/architecture.md) for the data flow, auth
chain and threading model.

## Development

Recipes are in the [`justfile`](justfile):

```sh
just run          # run the app
just ci           # fmt check, clippy -D warnings, tests, cargo-deny, docs
just coverage     # HTML coverage report (cargo-llvm-cov)
just bundle-macos # T3 Code.app and a .dmg in dist/ (macOS)
```

There are around 330 tests: protocol and contract round trips, ports of upstream's shell and
projection reducer tests, the sidebar's sections, ordering and labels, tool-group summaries and
run folds, markdown parsing, transport tests against an in-process WebSocket server, sync engine
reconnect and resume scenarios, a property test for the editor, and supervisor and attach tests
that run a fake Node server (skipped when `node` is missing).


## CI

`.github/workflows/desktop-rust.yml` runs format, clippy and tests on macOS, Linux and Windows,
rustdoc with warnings denied, and `cargo deny`, only when `native/desktop-rust/` changes.

## License

MIT, like the rest of the repository. The UI icons are from [Lucide](https://lucide.dev) (ISC); see
`crates/t3-desktop/assets/icons/LICENSE`.
