//! The `EXAMPLES` block at the foot of each subcommand's `--help`.
//!
//! One constant per command, kept together so that a reader can see at a glance
//! whether a command has one — and so a test can say that every command does.
//! Each example is something that would run, against a library laid out the way
//! the README's is, and the dangerous ones are `--dry-run` first because that is
//! the order the documentation asks the user to work in.

pub const CONFIG: &str = "Examples:
  mpdfm config show";

pub const CONFIG_SHOW: &str = "Examples:
  mpdfm config show
  mpdfm --music-dir /srv/music config show    # what a flag would change
  mpdfm config show --json";

pub const SCAN: &str = "Examples:
  mpdfm scan
  mpdfm scan --json";

pub const DOCTOR: &str = "Examples:
  mpdfm doctor
  mpdfm doctor --check references          # one group of checks
  mpdfm doctor --full                      # every finding, not the first ten
  mpdfm doctor --deep                      # also find byte-identical files (slow)";

pub const MOVE: &str = "Examples:
  mpdfm move --dry-run \"hiphop/MF DOOM\" \"hiphop/Doom\"    # look first
  mpdfm move \"hiphop/MF DOOM\" \"hiphop/Doom\"              # then do it
  mpdfm undo                                               # and take it back";

pub const ORGANIZE: &str = "Examples:
  mpdfm organize --dry-run --limit 20                       # a cautious first look
  mpdfm organize --dry-run rock/ -t \"{albumartist}/{album}/{track:02} {title}\"
  mpdfm organize --only-missing                             # work through it in stages";

pub const TAG: &str = "Examples:
  mpdfm tag show \"rock/Album\"
  mpdfm tag diff --genre \"Hip Hop\" -r \"hiphop/Album\"
  mpdfm tag set  --genre \"Hip Hop\" -r \"hiphop/Album\"";

pub const TAG_SHOW: &str = "Examples:
  mpdfm tag show \"rock/Album/01.mp3\"
  mpdfm tag show -r \"rock/Album\" --json";

pub const TAG_SET: &str = "Examples:
  mpdfm tag set --genre \"Hip Hop\" -r \"hiphop/Album\"
  mpdfm tag set --renumber-tracks \"rock/Album\"
  mpdfm tag set --clear comment --title-from-filename \"rock/Album/01.mp3\"";

pub const TAG_DIFF: &str = "Examples:
  mpdfm tag diff --genre \"Hip Hop\" -r \"hiphop/Album\"
  mpdfm tag diff --trim-whitespace -r \"rock\"";

pub const COVER: &str = "Examples:
  mpdfm cover show \"rock/Album\"
  mpdfm cover set --dry-run \"rock/Album\"";

pub const COVER_SHOW: &str = "Examples:
  mpdfm cover show \"rock/Album\"
  mpdfm cover show \"rock/Album/01.flac\" --json";

pub const COVER_SET: &str = "Examples:
  mpdfm cover set --dry-run \"rock/Album\"                  # uses cover.jpg / folder.jpg / front.jpg
  mpdfm cover set \"rock/Album\" scans/front.png --max-dimension 1000";

pub const COVER_EXTRACT: &str = "Examples:
  mpdfm cover extract --dry-run \"rock/Album\"
  mpdfm cover extract \"rock/Album\" --name folder.jpg";

pub const COVER_REMOVE: &str = "Examples:
  mpdfm cover remove --dry-run \"rock/Album\"";

pub const UNDO: &str = "Examples:
  mpdfm undo --list                         # what can be undone
  mpdfm undo                                # the most recent
  mpdfm undo 20260924T224500Z-00a3f1        # a specific one";

pub const RECOVER: &str = "Examples:
  mpdfm recover                              # survey, then roll back what a crash left
  mpdfm recover --forward                    # finish it instead
  mpdfm recover 20260924T224500Z-00a3f1

If mpdfm itself will not run, docs/SAFETY.md has the same recovery by hand.";

pub const COMPLETIONS: &str = "Examples:
  mpdfm completions zsh > ~/.zfunc/_mpdfm      # with ~/.zfunc on $fpath before compinit
  mpdfm completions bash > ~/.local/share/bash-completion/completions/mpdfm
  mpdfm completions fish > ~/.config/fish/completions/mpdfm.fish";

pub const MAN: &str = "Examples:
  mpdfm man | man -l -                       # read it without installing
  mpdfm man --dir ~/.local/share/man/man1    # mpdfm.1 and one page per subcommand";
