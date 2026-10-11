# mpdfm tag

```text
Read and write audio tags

Usage: mpdfm tag [OPTIONS] <COMMAND>

Commands:
  show  Print the tags of a file, or of every audio file in a directory
  set   Write tags to files or whole directories, previewing the change first
  diff  Show what `tag set` would change, and write nothing
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm tag show "rock/Album"
  mpdfm tag diff --genre "Hip Hop" -r "hiphop/Album"
  mpdfm tag set  --genre "Hip Hop" -r "hiphop/Album"
```

## mpdfm tag show

```text
Print the tags of a file, or of every audio file in a directory

Usage: mpdfm tag show [OPTIONS] <PATHS>...

Arguments:
  <PATHS>...  Files or directories to read

Options:
  -r, --recursive  Include audio files in subdirectories
  -h, --help       Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm tag show "rock/Album/01.mp3"
  mpdfm tag show -r "rock/Album" --json
```

## mpdfm tag set

```text
Write tags to files or whole directories, previewing the change first

Usage: mpdfm tag set [OPTIONS] <PATHS>...

Arguments:
  <PATHS>...  Files or directories to write

Options:
      --title <VALUE>             Track title
      --artist <VALUE>            Track artist
      --album-artist <VALUE>      Album artist
      --album <VALUE>             Album name
      --year <VALUE>              Release year
      --track <VALUE>             Track number
      --disc <VALUE>              Disc number
      --genre <VALUE>             Genre
      --comment <VALUE>           Comment
      --composer <VALUE>          Composer
      --clear <FIELD>             Remove a field entirely; repeatable
      --renumber-tracks           Number the selected files 1..n in the order they are listed, and set the total on each
      --title-from-filename       Take each file's title from its own name, stripping a leading track number and the extension
      --album-artist-from-artist  Give each file its own artist as its album artist
      --strip-comment             Remove the comment from every selected file
      --trim-whitespace           Strip leading and trailing space from every text field that has any
  -r, --recursive                 Include audio files in subdirectories
      --dry-run                   Show the preview and write nothing
  -y, --yes                       Skip the confirmation prompt. Required when stdin is not a terminal
  -h, --help                      Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm tag set --genre "Hip Hop" -r "hiphop/Album"
  mpdfm tag set --renumber-tracks "rock/Album"
  mpdfm tag set --clear comment --title-from-filename "rock/Album/01.mp3"
```

## mpdfm tag diff

```text
Show what `tag set` would change, and write nothing

Usage: mpdfm tag diff [OPTIONS] <PATHS>...

Arguments:
  <PATHS>...  Files or directories to write

Options:
      --title <VALUE>             Track title
      --artist <VALUE>            Track artist
      --album-artist <VALUE>      Album artist
      --album <VALUE>             Album name
      --year <VALUE>              Release year
      --track <VALUE>             Track number
      --disc <VALUE>              Disc number
      --genre <VALUE>             Genre
      --comment <VALUE>           Comment
      --composer <VALUE>          Composer
      --clear <FIELD>             Remove a field entirely; repeatable
      --renumber-tracks           Number the selected files 1..n in the order they are listed, and set the total on each
      --title-from-filename       Take each file's title from its own name, stripping a leading track number and the extension
      --album-artist-from-artist  Give each file its own artist as its album artist
      --strip-comment             Remove the comment from every selected file
      --trim-whitespace           Strip leading and trailing space from every text field that has any
  -r, --recursive                 Include audio files in subdirectories
      --dry-run                   Show the preview and write nothing
  -y, --yes                       Skip the confirmation prompt. Required when stdin is not a terminal
  -h, --help                      Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm tag diff --genre "Hip Hop" -r "hiphop/Album"
  mpdfm tag diff --trim-whitespace -r "rock"
```

