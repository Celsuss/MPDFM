# Recorded MPD conversations

Captured from **MPD 0.24.0** on `127.0.0.1:6600` (the setup `docs/PLAN.md` §3
describes) by sending each command on a raw socket and writing down every byte
that came back. They are what `tests/mpd.rs` drives the client with, so the
parser is tested against bytes a real daemon produced rather than against bytes
its author believed it produces — and so CI needs no daemon.

The format is documented on `mpdfm_core::testing::Transcript`:

| Prefix | Meaning |
| --- | --- |
| `S: text` | the daemon sent `text` and a newline |
| `S:` | the daemon sent an empty line |
| `S\| text` | the daemon sent `text` with no newline (a response cut short) |
| `C: text` | the client is expected to send `text` and a newline |
| `#…` | a comment |

Where a file contains something that was **not** captured — a response this
daemon would not produce on demand, like an `ACK [50@…]` or a stream in the
queue — the comment above it says so and names where the shape comes from.

To re-record, with `mpd` running:

```sh
python3 - <<'EOF'
import socket
s = socket.create_connection(("127.0.0.1", 6600), 2); s.settimeout(2)
f = s.makefile("rb")
print(repr(f.readline()))
for cmd in [b"status", b"currentsong", b"playlistinfo"]:
    s.sendall(cmd + b"\n")
    while True:
        line = f.readline()
        print(repr(line))
        if not line or line[:2] in (b"OK", b"AC"):
            break
EOF
```
