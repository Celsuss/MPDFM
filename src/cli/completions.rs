//! `mpdfm completions` and `mpdfm man`: the two outputs generated from the clap
//! definitions, so neither can drift from what the program accepts.
//!
//! They are subcommands and not a `build.rs` step because the argument
//! definitions live in this crate's binary, which a build script cannot link
//! against; and a subcommand is what a package build, `just docs` and a user who
//! cloned the repo can all run the same way.

use std::io::Write as _;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use camino::Utf8Path;
use clap::{Command, CommandFactory as _};

use super::{Cli, CompletionsArgs, ManArgs};
use crate::output::Exit;

/// `mpdfm completions <SHELL>`: the script, on stdout.
pub fn script(args: &CompletionsArgs) -> Result<ExitCode> {
    let mut cmd = Cli::command();
    // Into memory first: `generate` panics on a write error, and `mpdfm
    // completions zsh | head` closing the pipe is an error, not a bug.
    let mut script = Vec::new();
    clap_complete::generate(args.shell, &mut cmd, "mpdfm", &mut script);
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&script)
        .and_then(|()| stdout.flush())
        .context("writing the completion script")?;
    Ok(Exit::Ok.into())
}

/// `mpdfm man [--dir DIR]`.
pub fn man(args: &ManArgs) -> Result<ExitCode> {
    let pages = pages()?;
    match &args.dir {
        None => {
            let (_, root) = &pages[0];
            std::io::stdout()
                .lock()
                .write_all(root)
                .context("writing the man page")?;
        }
        Some(dir) => write_pages(dir, &pages)?,
    }
    Ok(Exit::Ok.into())
}

fn write_pages(dir: &Utf8Path, pages: &[(String, Vec<u8>)]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
    for (name, roff) in pages {
        let path = dir.join(format!("{name}.1"));
        std::fs::write(&path, roff).with_context(|| format!("writing {path}"))?;
    }
    Ok(())
}

/// Every page: `mpdfm` first, then one per subcommand at any depth, named the way
/// `git` names its own (`mpdfm-tag-set`).
///
/// Returned rather than written so a test can read them without a filesystem.
pub fn pages() -> Result<Vec<(String, Vec<u8>)>> {
    let mut root = Cli::command();
    // Without this the global flags are not yet copied onto the subcommands, and
    // every page but the first would leave out `--music-dir` and `--json`.
    root.build();

    let mut pages = Vec::new();
    collect(&root, "mpdfm", &mut pages)?;
    Ok(pages)
}

fn collect(cmd: &Command, name: &str, pages: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
    pages.push((name.to_owned(), render(cmd, name)?));
    for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
        collect(sub, &format!("{name}-{}", sub.get_name()), pages)?;
    }
    Ok(())
}

/// One page, section by section, so that the hand-written parts sit where a man
/// page keeps them and not under the `EXTRA` heading `clap_mangen` would give
/// `after_help`.
///
/// `build()` has already given each subcommand the display name `mpdfm-tag-set`
/// and the bin name `mpdfm tag set`, which is what the title and synopsis want.
fn render(cmd: &Command, name: &str) -> Result<Vec<u8>> {
    let man = clap_mangen::Man::new(cmd.clone());
    let mut buf = Vec::new();
    let io = |e: std::io::Error| anyhow::anyhow!("rendering the man page for {name}: {e}");

    man.render_title(&mut buf).map_err(io)?;
    man.render_name_section(&mut buf).map_err(io)?;
    man.render_synopsis_section(&mut buf).map_err(io)?;
    man.render_description_section(&mut buf).map_err(io)?;
    man.render_options_section(&mut buf).map_err(io)?;

    // Not `render_subcommands_section`: it lists `help`, which has no page, and it
    // writes an empty heading for a command that has only that.
    let subs: Vec<&Command> = cmd
        .get_subcommands()
        .filter(|s| s.get_name() != "help")
        .collect();
    if !subs.is_empty() {
        buf.extend_from_slice(b".SH SUBCOMMANDS\n");
        for sub in subs {
            let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
            let line = format!(
                ".TP\n\\fB{name}\\-{}\\fR(1)\n{}\n",
                sub.get_name(),
                about.replace('-', "\\-")
            );
            buf.extend_from_slice(line.as_bytes());
        }
    }

    let is_root = name == "mpdfm";
    let after = cmd
        .get_after_help()
        .map(ToString::to_string)
        .unwrap_or_default();
    if is_root {
        section(&mut buf, "EXIT STATUS", &after);
        buf.extend_from_slice(ROOT_TAIL.as_bytes());
    } else if let Some(examples) = after.strip_prefix("Examples:") {
        // The part of `--help` that is not prose, so it is not reflowed.
        section(&mut buf, "EXAMPLES", examples);
    }

    let mut see_also: Vec<String> = Vec::new();
    if !is_root {
        see_also.push("mpdfm(1)".to_owned());
    }
    see_also.extend(
        cmd.get_subcommands()
            .filter(|s| s.get_name() != "help")
            .map(|s| format!("{name}-{}(1)", s.get_name())),
    );
    if is_root {
        see_also.push("mpd(1)".to_owned());
    }
    buf.extend_from_slice(b".SH \"SEE ALSO\"\n");
    buf.extend_from_slice(see_also.join(", ").replace('-', "\\-").as_bytes());
    buf.extend_from_slice(b"\n");
    // Only the top command carries a version; the section unwraps it.
    if is_root {
        man.render_version_section(&mut buf).map_err(io)?;
    }
    Ok(ascii(&buf))
}

/// roff is read as bytes in whatever the reader's locale is; `\\[uXXXX]` names the
/// character without depending on it, which is what keeps a `—` from becoming two
/// garbage glyphs under `man` and a warning under `groff`.
fn ascii(roff: &[u8]) -> Vec<u8> {
    let mut out = String::new();
    for c in String::from_utf8_lossy(roff).chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            out.push_str(&format!("\\[u{:04X}]", u32::from(c)));
        }
    }
    out.into_bytes()
}

/// A section whose body keeps its line breaks.
fn section(buf: &mut Vec<u8>, title: &str, body: &str) {
    buf.extend_from_slice(format!(".SH \"{title}\"\n.nf\n").as_bytes());
    for line in body.trim_matches('\n').lines() {
        let escaped = line.replace('\\', "\\e").replace('-', "\\-");
        // A line that starts with `.` or `'` is a request to roff, not text.
        if escaped.starts_with(['.', '\'']) {
            buf.extend_from_slice(b"\\&");
        }
        buf.extend_from_slice(escaped.as_bytes());
        buf.push(b'\n');
    }
    buf.extend_from_slice(b".fi\n");
}

/// What only the top page has: where things live, which the argument definitions
/// do not know. Written by hand, so `docs/SAFETY.md` and `docs/config.example.toml`
/// are named rather than repeated.
const ROOT_TAIL: &str = r#".SH FILES
.TP
.I $XDG_CONFIG_HOME/mpdfm/config.toml
Settings. Every one is optional; see \fBmpdfm config show\fR for what each resolved to
and why. Documented in full in docs/config.example.toml.
.TP
.I $XDG_CONFIG_HOME/mpdfm/keys.toml
The TUI key bindings, merged over the built\-in vim\-style ones. Documented in
docs/keys.example.toml.
.TP
.I $XDG_DATA_HOME/mpdfm/journal/
One record per transaction. \fBundo\fR and \fBrecover\fR read these.
.TP
.I $XDG_DATA_HOME/mpdfm/backups/<txid>/
The playlists, MPD's state file, and the bytes of anything deleted or rewritten, as
they were before that transaction.
.SH ENVIRONMENT
.TP
.B XDG_CONFIG_HOME, XDG_DATA_HOME
Where the above live; \fI~/.config\fR and \fI~/.local/share\fR when unset.
.SH "SAFETY MODEL"
MPD stores a track's path in every playlist and in its saved queue, so moving a
file with \fBmv\fR breaks both. Every command that changes the library previews first
(\fB\-\-dry\-run\fR shows the same preview and writes nothing), journals the transaction
durably before the first change, and can be reversed byte for byte with
\fBmpdfm undo\fR. If a crash interrupts a commit, \fBmpdfm recover\fR finishes or
rolls it back. The recovery procedure to follow by hand, if \fBmpdfm\fR itself will not
run, is in docs/SAFETY.md.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_one_page_per_command_and_the_first_is_the_top() {
        let pages = pages().unwrap();
        let names: Vec<&str> = pages.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names[0], "mpdfm");
        for expected in [
            "mpdfm-move",
            "mpdfm-organize",
            "mpdfm-tag-set",
            "mpdfm-cover-extract",
            "mpdfm-config-show",
            "mpdfm-recover",
            "mpdfm-completions",
        ] {
            assert!(
                names.contains(&expected),
                "no page for {expected}: {names:?}"
            );
        }
        assert!(!names.iter().any(|n| n.contains("help")), "{names:?}");
    }

    #[test]
    fn the_top_page_covers_every_subcommand_the_exit_codes_and_the_files() {
        let pages = pages().unwrap();
        let top = String::from_utf8(pages[0].1.clone()).unwrap();
        for sub in Cli::command()
            .get_subcommands()
            .filter(|s| s.get_name() != "help")
        {
            assert!(
                top.contains(&format!("mpdfm\\-{}", sub.get_name())),
                "mpdfm.1 does not list `{}`",
                sub.get_name()
            );
        }
        for section in [
            "EXIT STATUS",
            "FILES",
            "SAFETY MODEL",
            "SEE ALSO",
            "VERSION",
        ] {
            assert!(
                top.contains(&format!(".SH \"{section}\""))
                    || top.contains(&format!(".SH {section}")),
                "{section}"
            );
        }
    }

    #[test]
    fn a_subcommand_page_documents_the_global_flags_it_inherits() {
        let pages = pages().unwrap();
        let (_, move_page) = pages.iter().find(|(n, _)| n == "mpdfm-move").unwrap();
        let text = String::from_utf8(move_page.clone()).unwrap();
        assert!(text.contains("music\\-dir"), "{text}");
        assert!(text.contains("dry\\-run"), "{text}");
        assert!(text.contains(".SH \"EXAMPLES\""), "{text}");
    }

    #[test]
    fn a_page_is_plain_ascii_whatever_the_help_text_contains() {
        for (name, roff) in pages().unwrap() {
            assert!(roff.is_ascii(), "{name} has a non-ASCII byte");
        }
    }

    #[test]
    fn every_page_is_clean_roff() {
        // `groff` is what `man` is built on. Absent on a minimal machine, in which
        // case the rest of these tests are what there is.
        let Ok(probe) = std::process::Command::new("groff")
            .arg("--version")
            .output()
        else {
            return;
        };
        if !probe.status.success() {
            return;
        }
        for (name, roff) in pages().unwrap() {
            let mut child = std::process::Command::new("groff")
                .args(["-man", "-Tutf8", "-ww", "-z"])
                .stdin(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&roff).unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success() && out.stderr.is_empty(),
                "groff on {name}.1:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn the_zsh_script_completes_subcommands_and_flags() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(clap_complete::Shell::Zsh, &mut cmd, "mpdfm", &mut buf);
        let script = String::from_utf8(buf).unwrap();
        assert!(script.starts_with("#compdef mpdfm"), "{}", &script[..40]);
        for word in [
            "organize",
            "recover",
            "completions",
            "--dry-run",
            "--music-dir",
            "--only-missing",
        ] {
            assert!(script.contains(word), "zsh script lacks {word}");
        }
    }
}
