#!/bin/sh
# Write one Markdown page per top-level command into book/src/reference/cli/,
# from the binary's own --help output, so the site cannot drift from the code.
# Usage: scripts/gen-cli-reference.sh [path/to/mpdfm]
set -eu

bin=${1:-./target/debug/mpdfm}
out=book/src/reference/cli
mkdir -p "$out"

page() {
    name=$1
    shift
    file=$out/$name.md
    {
        printf '# mpdfm %s\n\n' "$name"
        for sub in "$@"; do
            [ -n "$sub" ] && printf '## mpdfm %s %s\n\n' "$name" "$sub"
            printf '```text\n'
            if [ -n "$sub" ]; then "$bin" "$name" $sub --help; else "$bin" "$name" --help; fi
            printf '```\n\n'
        done
    } > "$file"
}

page config   "" show
page scan     ""
page doctor   ""
page move     ""
page organize ""
page tag      "" show set diff
page cover    "" show set extract remove
page undo     ""
page recover  ""
page completions ""
page man      ""

# The roadmap links to docs/tasks/*.md, which the site does not carry: point those
# at GitHub instead of leaving them dead.
sed 's#](tasks/#](https://github.com/Celsuss/MPDFM/blob/main/docs/tasks/#g' \
    docs/ROADMAP.md > book/src/design/roadmap.md
