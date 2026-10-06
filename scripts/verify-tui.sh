#!/bin/sh
# Measure what `cargo test` cannot: how much CPU an idle `mpdfm` uses, and whether
# a real terminal comes back unchanged. See docs/tasks/20-tui-shell.md.
#
# Both halves need a pseudo-terminal, which is what `script(1)` is here for; the
# process is inspected through /proc, so this is Linux-only, like MPD itself.
#
# The pty restore checks also run as `cargo test --test tui_terminal`. What is only
# here is the CPU number, because 30 seconds of sitting still is not something to
# put in a test suite.
set -eu

WATCH=${1:-30}
BIN=${2:-./target/release/mpdfm}

command -v script >/dev/null 2>&1 || {
    echo "verify-tui: needs \`script\` (util-linux)" >&2
    exit 1
}
[ -x "$BIN" ] || {
    echo "verify-tui: no binary at $BIN — run \`cargo build --release\` first" >&2
    exit 1
}
BIN=$(readlink -f "$BIN")
INNER=$(mktemp)
trap 'rm -f "$INNER"' EXIT

# Inside the pty: size it, remember the terminal's modes, start the TUI, read the
# process's own CPU counters either side of a long sleep, stop it with SIGTERM, and
# compare the modes with what they were.
cat > "$INNER" <<'INNER_EOF'
stty rows 30 cols 100
before=$(stty -g)
"$BIN" --no-mpd &
pid=$!
sleep 1
start=$(awk '{print $14 + $15}' "/proc/$pid/stat")
sleep "$WATCH"
end=$(awk '{print $14 + $15}' "/proc/$pid/stat")
threads=$(awk '/Threads/{print $2}' "/proc/$pid/status")
rss=$(awk '/VmRSS/{print $2}' "/proc/$pid/status")
kill -TERM "$pid"
wait "$pid"
code=$?
after=$(stty -g)
if [ "$before" = "$after" ]; then modes=unchanged; else modes="CHANGED"; fi
echo "MPDFM-VERIFY ticks=$((end - start)) hz=$(getconf CLK_TCK) threads=$threads rss_kb=$rss exit=$code modes=$modes"
INNER_EOF

export BIN WATCH
# stdin from /dev/null: nothing is typed, because the point is a session that is
# doing nothing at all.
# `grep -o`, not an anchored match: the recording is a terminal's worth of cursor
# motion, and the last frame's escape sequences sit on the same line as the first
# thing printed after the alternate screen is left.
result=$(timeout $((WATCH + 30)) script --quiet --return --command "sh $INNER" /dev/null \
    < /dev/null | tr -d '\r' | grep -o 'MPDFM-VERIFY .*' || true)

[ -n "$result" ] || {
    echo "verify-tui: the run produced no result line" >&2
    exit 1
}

for field in $result; do
    case $field in
        ticks=*) ticks=${field#ticks=} ;;
        hz=*) hz=${field#hz=} ;;
        threads=*) threads=${field#threads=} ;;
        rss_kb=*) rss=${field#rss_kb=} ;;
        exit=*) code=${field#exit=} ;;
        modes=*) modes=${field#modes=} ;;
    esac
done

awk -v t="$ticks" -v hz="$hz" -v w="$WATCH" 'BEGIN {
    printf "idle CPU:  %.3f s over %d s = %.2f%% of one core (%d clock ticks)\n",
        t / hz, w, 100 * t / hz / w, t
}'
echo "threads:   $threads (main, input, tick, signals)"
echo "resident:  $rss kB"
echo "SIGTERM:   exit $code"
echo "terminal:  $modes"

[ "$code" = 0 ] || {
    echo "verify-tui: SIGTERM did not exit cleanly" >&2
    exit 1
}
[ "$modes" = unchanged ] || {
    echo "verify-tui: the terminal was not restored" >&2
    exit 1
}
# One per cent of a core is two orders of magnitude above what a correct event loop
# costs and still well below what a polling one does, so it separates the two
# without being a benchmark of this machine.
awk -v t="$ticks" -v hz="$hz" -v w="$WATCH" 'BEGIN { exit (100 * t / hz / w < 1.0) ? 0 : 1 }' || {
    echo "verify-tui: idle CPU is above 1% of a core, so something is polling" >&2
    exit 1
}
echo "verify-tui: ok"
