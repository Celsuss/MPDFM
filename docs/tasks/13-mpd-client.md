# 13 — Minimal MPD protocol client

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 04
- **Status:** not started

## Goal

Talk to MPD just enough to keep its database fresh and to warn about conflicts,
without taking on an async runtime or a dependency we don't control (decision D6).

## Details

The protocol is newline-delimited text over TCP (or a Unix socket). A blocking
client on `std::net::TcpStream` with timeouts is enough:

```rust
struct Mpd { /* stream, reader */ }

impl Mpd {
    fn connect(addr: &MpdAddr, timeout: Duration) -> Result<Mpd, MpdError>;
    fn status(&mut self) -> Result<Status>;          // state, playlistlength, song
    fn current_song(&mut self) -> Result<Option<RelPath>>;
    fn queue_paths(&mut self) -> Result<Vec<RelPath>>;   // playlistinfo → file: lines
    fn update(&mut self, dir: Option<&RelPath>) -> Result<JobId>;
    fn rescan(&mut self, dir: Option<&RelPath>) -> Result<JobId>;
}
```

Details that matter:

- Greeting is `OK MPD <version>`; parse and expose the version.
- Responses are `key: value` lines terminated by `OK` or `ACK [code@idx] {cmd} msg`.
  Map `ACK` to a typed error carrying the code.
- Quote arguments correctly: MPD wants `update "hip hop/Some Album"` with `"`
  and `\` escaped. Album names here contain quotes, brackets, `&` and `+` — get
  this right and test it with the real names.
- Connection failure, timeout and mid-command disconnect are all **non-fatal**:
  return `Err` and let callers degrade. `--no-mpd` skips connecting entirely.
- Support `password` if configured (this config has none), and a Unix socket
  address form for portability.
- Never block the TUI: connect with a short timeout, and treat MPD state as
  cached best-effort info refreshed on demand.

Used by: task 11 (post-commit `update`), task 14 (queue conflict detection),
task 26 (status indicator).

## Acceptance criteria

- [ ] connects to the real daemon on `127.0.0.1:6600` and reports its version
- [ ] `update` with a path containing spaces, `[`, `]`, `&`, `+`, `'` and `"`
      succeeds — tested against a real daemon or a mock that asserts the wire bytes
- [ ] `ACK` responses become typed errors with the MPD error code
- [ ] connect to a closed port fails within the timeout and does not panic
- [ ] a daemon that disconnects mid-response yields an error, not a hang
- [ ] `queue_paths` returns the current queue as `RelPath`s
- [ ] a unit test drives the parser from recorded protocol transcripts (no daemon
      required in CI)
- [ ] `--no-mpd` prevents any socket from being opened (assert in a test)

## Files

`crates/core/src/mpd/{mod.rs,proto.rs,client.rs}`, `tests/transcripts/`

## Pitfalls

- `update` is asynchronous: it returns a job id and the DB is not current when
  the command returns. Don't report "MPD updated" — report "update queued".
- Updating the whole library takes noticeable time; pass the affected
  directories, deduplicated to the shallowest common ancestors.
