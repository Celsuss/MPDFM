# Install

MPDFM needs a Rust toolchain (1.93 or later) and nothing else — tag handling is
pure Rust, and MPD is optional.

```console
$ git clone https://github.com/Celsuss/MPDFM && cd MPDFM
$ cargo install --path .          # puts `mpdfm` in ~/.cargo/bin
```

## Man pages

Man pages are generated from the argument definitions, so they always match the
binary:

```console
$ mpdfm man | man -l -                                    # read it now
$ mpdfm man --dir ~/.local/share/man/man1                 # install: mpdfm.1 + one per subcommand
```

Afterwards `man mpdfm` and `man mpdfm-move` work.

## Shell completions

**zsh** — put the file anywhere on `$fpath` *before* `compinit`:

```console
$ mkdir -p ~/.zfunc
$ mpdfm completions zsh > ~/.zfunc/_mpdfm
```

```zsh
# ~/.zshrc, before compinit
fpath=(~/.zfunc $fpath)
autoload -Uz compinit && compinit
```

Open a new shell, and `mpdfm <TAB>` completes subcommands and `mpdfm move --<TAB>`
its flags. If it does not, `rm -f ~/.zcompdump*` and start another shell.

**bash**

```console
$ mpdfm completions bash > ~/.local/share/bash-completion/completions/mpdfm
```

**fish**

```console
$ mpdfm completions fish > ~/.config/fish/completions/mpdfm.fish
```

`elvish` and `powershell` are also supported.
