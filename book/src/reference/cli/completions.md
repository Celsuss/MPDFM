# mpdfm completions

```text
Print a shell completion script (bash, zsh, fish, elvish, powershell)

Usage: mpdfm completions [OPTIONS] <SHELL>

Arguments:
  <SHELL>  The shell to generate for [possible values: bash, elvish, fish, powershell, zsh]

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
  mpdfm completions zsh > ~/.zfunc/_mpdfm      # with ~/.zfunc on $fpath before compinit
  mpdfm completions bash > ~/.local/share/bash-completion/completions/mpdfm
  mpdfm completions fish > ~/.config/fish/completions/mpdfm.fish
```

