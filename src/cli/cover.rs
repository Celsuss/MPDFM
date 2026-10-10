//! `mpdfm cover show | set | extract | remove` — task 30.
//!
//! Assembly, like `tag`: core's [`art`] reads and writes the pictures, and every
//! write is an ordinary plan — `WriteTags` carrying a cover edit for `set` and
//! `remove`, `ExtractArt` for `extract` — so it is previewed, refused as a whole
//! if any file in it cannot be written, journaled, and undone by `mpdfm undo`.
//!
//! ```text
//! 1  resolve ALBUM_DIR, and take the audio files directly in it
//! 2  read their pictures
//! 3  work out the plan — skipping tracks that already say what was asked
//! 4  validate, print the preview, and for `set` what it costs in bytes
//! 5  refused / --dry-run / --yes / ask — the same gates as `tag set`
//! 6  commit, and print the txid
//! ```
//!
//! # What `set` costs
//!
//! A cover is embedded once **per track**, so a 5 MB scan into a 14-track album
//! is 70 MB. The preview says so before asking — the size the library grows or
//! shrinks by, net of the front covers being replaced — and suggests
//! `--max-dimension` when the image is over a megabyte, because that is the
//! common scene-release case and the user may not know the cover is that big.

use std::process::ExitCode;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::art::{self, AlbumArt, CoverImage, Embedded, ImageFormat, human_bytes};
use mpdfm_core::config::Config;
use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::ops::commit::{self, Previewed};
use mpdfm_core::ops::{Committed, Effects, Operation, Plan};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::tags::TagDelta;

use super::{Cli, CoverExtractArgs, CoverRemoveArgs, CoverSetArgs, CoverShowArgs, CoverWriteFlags};
use crate::output::{self, Exit, Out, Style};

/// An image over this size gets the `--max-dimension` hint: the task's
/// threshold, and where a cover stops being a cover and starts being a scan.
const LARGE_COVER: u64 = 1_000_000;

/// `mpdfm cover show <PATH>` — what art a track or an album has, and where the
/// embedded and the loose disagree.
///
/// # Errors
///
/// A missing library root, or a `PATH` that is not a track or a directory in it.
pub fn show(cli: &Cli, config: &Config, out: &Out, args: &CoverShowArgs) -> Result<ExitCode> {
    let root = config.require_music_dir()?;
    let library = Library::scan(root)?;
    let rel = super::inside(&args.path, root)?;
    let dir = DirPath::from(rel.clone());

    let survey = if library.dir(&dir).is_some() {
        art::album(&library, &dir).with_context(|| format!("cannot list {dir}"))?
    } else {
        anyhow::ensure!(
            library.get(&rel).is_some_and(|entry| entry.is_audio()),
            "{rel} is not a track or a directory in the library"
        );
        // One track: its own pictures, and the images beside it.
        let parent = DirPath::of(&rel);
        let mut survey =
            art::album(&library, &parent).with_context(|| format!("cannot list {parent}"))?;
        survey.tracks.retain(|track| track.rel == rel);
        survey.mismatches = art::read::mismatches(&survey.tracks, &survey.loose);
        survey
    };
    cli.trace(format!(
        "cover show: {} track(s), {} loose image(s)",
        survey.tracks.len(),
        survey.loose.len()
    ));

    if out.json {
        output::json(&serde_json::to_value(&survey)?)?;
        return Ok(Exit::Ok.into());
    }
    print_survey(&survey, out);
    let failed = survey.tracks.iter().any(|track| track.error.is_some());
    Ok(if failed { Exit::Error } else { Exit::Ok }.into())
}

/// `mpdfm cover set <ALBUM_DIR> [IMAGE]`.
///
/// # Errors
///
/// As [`show`], plus an image that cannot be read or is not a JPEG or PNG, and
/// anything a commit raises that is not a refusal.
pub fn set(cli: &Cli, config: &Config, out: &Out, args: &CoverSetArgs) -> Result<ExitCode> {
    let album = Album::open(config, &args.album_dir)?;
    let source = image_path(&album, args.image.as_deref())?;
    let (mut image, bytes) = CoverImage::load(&source)?;
    cli.trace(format!("cover set: {} ({})", image.source, image.image));

    let mut notes = Vec::new();
    if let Some(max) = args.max_dimension
        && let Some(smaller) = art::downscale(&bytes, max.get(), &image.source)?
    {
        let staged = art::stage(&config.data_dir, &smaller)?;
        let (resized, _) = CoverImage::load(&staged)?;
        notes.push(format!(
            "{} was downscaled to fit {max} px: {} → {}",
            image.source.file_name().unwrap_or("the image"),
            image.image,
            resized.image
        ));
        image = resized;
    }

    // Tracks already carrying exactly this front cover, and nothing else in
    // front, need no write.
    let pictures = album.pictures();
    let mut targets = Vec::new();
    let mut growth: i128 = 0;
    for (rel, pictures) in &pictures {
        let fronts: Vec<&Embedded> = pictures.iter().filter(|p| p.kind.is_front()).collect();
        if let [front] = fronts.as_slice()
            && front.image.hash == image.image.hash
        {
            continue;
        }
        growth += i128::from(image.image.size)
            - fronts
                .iter()
                .map(|p| i128::from(p.image.size))
                .sum::<i128>();
        targets.push(rel.clone());
    }
    if targets.is_empty() {
        return nothing(out, "Every track already has that front cover.");
    }

    notes.push(format!(
        "embedding a {} image into {} adds {} to the library{}",
        human_bytes(image.image.size),
        plural(targets.len(), "file"),
        signed_bytes(growth),
        if growth < 0 {
            " (it replaces larger covers)"
        } else {
            ""
        }
    ));
    if image.image.size > LARGE_COVER && args.max_dimension.is_none() {
        notes.push("the image is over 1 MB; `--max-dimension 1000` would shrink it".to_owned());
    }

    let delta = TagDelta::new().set_cover(image);
    let plan = Plan::of(
        targets
            .into_iter()
            .map(|target| Operation::WriteTags {
                target,
                changes: delta.clone(),
            })
            .collect(),
    );
    run(
        cli,
        config,
        out,
        &album,
        &plan,
        &args.flags,
        &notes,
        "Embed this cover?",
    )
}

/// `mpdfm cover extract <ALBUM_DIR> [--name NAME]`.
///
/// # Errors
///
/// As [`show`], plus an album none of whose tracks embeds a picture, and a name
/// that is not a plain file name.
pub fn extract(cli: &Cli, config: &Config, out: &Out, args: &CoverExtractArgs) -> Result<ExitCode> {
    let album = Album::open(config, &args.album_dir)?;
    let pictures = album.pictures();
    let Some((from, picture)) = pictures
        .iter()
        .find_map(|(rel, pictures)| art::read::preferred(pictures).map(|p| (rel, p)))
    else {
        anyhow::bail!("no track in {} embeds a picture to extract", album.dir);
    };

    let extension = picture.image.format.extension();
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("cover.{extension}"));
    anyhow::ensure!(
        !name.is_empty() && !name.contains('/') && name != "." && name != "..",
        "--name takes a file name, not a path: {name:?}"
    );
    let to = album
        .dir
        .join(&name)
        .with_context(|| format!("{name:?} cannot be a file name in {}", album.dir))?;

    let mut notes = vec![format!(
        "the {} of {} — {}, written byte for byte",
        picture.kind,
        from.file_name(),
        picture.image
    )];
    let named = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    let fits = match picture.image.format {
        ImageFormat::Jpeg => matches!(named.as_deref(), Some("jpg" | "jpeg")),
        ImageFormat::Tiff => matches!(named.as_deref(), Some("tif" | "tiff")),
        format => named.as_deref() == Some(format.extension()),
    };
    if !fits {
        notes.push(format!(
            "the picture is {} but {name} says otherwise; a player trusting the name \
             may not open it",
            picture.image.format
        ));
    }

    // Already there, and the same bytes: nothing to do, and not a conflict.
    let existing = to.to_abs(album.library.root());
    if std::fs::read(&existing)
        .is_ok_and(|bytes| mpdfm_core::ops::exec_fs::hash_bytes(&bytes) == picture.image.hash)
    {
        return nothing(out, &format!("{to} already holds that picture."));
    }

    let plan = Plan::of(vec![Operation::ExtractArt {
        from: from.clone(),
        to,
        picture: picture.image,
    }]);
    run(
        cli,
        config,
        out,
        &album,
        &plan,
        &args.flags,
        &notes,
        "Write this file?",
    )
}

/// `mpdfm cover remove <ALBUM_DIR>`.
///
/// # Errors
///
/// As [`show`], plus anything a commit raises that is not a refusal.
pub fn remove(cli: &Cli, config: &Config, out: &Out, args: &CoverRemoveArgs) -> Result<ExitCode> {
    let album = Album::open(config, &args.album_dir)?;
    let mut freed = 0u64;
    let targets: Vec<RelPath> = album
        .pictures()
        .into_iter()
        .filter_map(|(rel, pictures)| {
            let fronts: u64 = pictures
                .iter()
                .filter(|p| p.kind.is_front())
                .map(|p| p.image.size)
                .sum();
            freed += fronts;
            (fronts > 0).then_some(rel)
        })
        .collect();
    if targets.is_empty() {
        return nothing(out, "No track in that album embeds a front cover.");
    }
    let notes = vec![format!(
        "removing the front cover from {} frees {}; other pictures stay",
        plural(targets.len(), "file"),
        human_bytes(freed)
    )];
    let delta = TagDelta::new().remove_cover();
    let plan = Plan::of(
        targets
            .into_iter()
            .map(|target| Operation::WriteTags {
                target,
                changes: delta.clone(),
            })
            .collect(),
    );
    run(
        cli,
        config,
        out,
        &album,
        &plan,
        &args.flags,
        &notes,
        "Remove these covers?",
    )
}

// ---------------------------------------------------------------------------

/// An album directory and its tracks.
struct Album {
    library: Library,
    dir: DirPath,
    tracks: Vec<RelPath>,
}

impl Album {
    /// Resolve `raw` to an album directory and take the audio files directly in
    /// it.
    fn open(config: &Config, raw: &Utf8Path) -> Result<Self> {
        let root = config.require_music_dir()?;
        let library = Library::scan(root)?;
        let rel = super::inside(raw, root)?;
        let dir = DirPath::from(rel);
        anyhow::ensure!(
            library.dir(&dir).is_some(),
            "{dir} is not a directory in the library; run `mpdfm scan` if it should be"
        );
        let tracks: Vec<RelPath> = library
            .files_in(&dir)
            .filter(|entry| entry.is_audio())
            .map(|entry| entry.rel.clone())
            .collect();
        anyhow::ensure!(
            !tracks.is_empty(),
            "{dir} holds no audio files directly — name a disc directory, not the set"
        );
        Ok(Self {
            library,
            dir,
            tracks,
        })
    }

    /// Every track's pictures. A track that cannot be read contributes none,
    /// and is then refused by the plan's own preflight if it is written to.
    fn pictures(&self) -> Vec<(RelPath, Vec<Embedded>)> {
        let root = self.library.root();
        self.tracks
            .iter()
            .map(|rel| {
                (
                    rel.clone(),
                    art::embedded(&rel.to_abs(root)).unwrap_or_default(),
                )
            })
            .collect()
    }
}

/// The image `cover set` was given: a path as typed, or the name of an image in
/// the album directory — or, with nothing given, the album's best loose image.
fn image_path(album: &Album, given: Option<&Utf8Path>) -> Result<Utf8PathBuf> {
    let dir = album.dir.to_abs(album.library.root());
    if let Some(given) = given {
        if given.is_file() {
            return Ok(given.to_owned());
        }
        let beside = dir.join(given);
        anyhow::ensure!(beside.is_file(), "{given} is not a file, here or in {dir}");
        return Ok(beside);
    }
    let loose = art::loose(&dir).with_context(|| format!("cannot list {dir}"))?;
    let best = loose
        .iter()
        .find(|file| file.image.format.embeddable())
        .with_context(|| format!("{dir} holds no JPEG or PNG to embed; name the image to use"))?;
    Ok(art::read::loose_path(&dir, best))
}

/// Validate, preview, decide, commit — the part every cover write shares.
#[allow(clippy::too_many_arguments)]
fn run(
    cli: &Cli,
    config: &Config,
    out: &Out,
    album: &Album,
    plan: &Plan,
    flags: &CoverWriteFlags,
    notes: &[String],
    question: &str,
) -> Result<ExitCode> {
    let (index, index_warnings) = PlaylistIndex::load(&config.playlist_dir);
    for warning in &index_warnings {
        cli.trace(warning);
    }
    let effects = plan.validate(&album.library, &index, config);

    if !out.json {
        println!("{}", effects.render(out.width));
        if !notes.is_empty() {
            println!();
        }
        for note in notes {
            println!("{} {note}", out.paint(Style::Yellow, "!"));
        }
    }

    let stop = |exit: Exit| -> Result<ExitCode> {
        if out.json {
            output::json(&report(&effects, notes, None, exit))?;
        }
        Ok(exit.into())
    };
    if !effects.conflicts.is_empty() {
        if !out.json {
            println!();
            println!("{}", out.paint(Style::Red, "Nothing was changed."));
        }
        return stop(Exit::Conflict);
    }
    if flags.dry_run {
        if !out.json {
            println!();
            println!(
                "{}",
                out.paint(Style::Dim, "--dry-run: nothing was changed.")
            );
        }
        return stop(Exit::Ok);
    }
    if flags.yes {
        cli.trace("--yes: committing without asking");
    } else {
        println!();
        if !out.confirm(question)? {
            println!("{}", out.paint(Style::Dim, "Nothing was changed."));
            return stop(Exit::Declined);
        }
    }

    let link = super::mpd::Link::open(cli, config);
    let update = |dirs: &[DirPath]| link.update(dirs);
    let options = commit::Options {
        update: link.connected().then_some(&update as commit::Updater<'_>),
        ..commit::Options::default()
    };
    let previewed = Previewed {
        plan,
        library: &album.library,
        effects: &effects,
    };
    match commit::commit_with(&previewed, config, &options) {
        Ok(committed) => {
            if out.json {
                output::json(&report(&effects, notes, Some(&committed), Exit::Ok))?;
            } else {
                super::tag::print_committed(&committed, out);
            }
            Ok(Exit::Ok.into())
        }
        Err(err) if super::tag::refused_without_writing(&err) => {
            if out.json {
                let mut document = report(&effects, notes, None, Exit::Conflict);
                document["error"] = serde_json::Value::String(err.to_string());
                output::json(&document)?;
            } else {
                eprintln!("mpdfm: {err}");
            }
            Ok(Exit::Conflict.into())
        }
        Err(err) => Err(err.into()),
    }
}

/// A write with nothing left to do: say so, and succeed.
fn nothing(out: &Out, message: &str) -> Result<ExitCode> {
    if out.json {
        output::json(&serde_json::json!({ "message": message, "exit": Exit::Ok.code() }))?;
    } else {
        println!("{message}");
    }
    Ok(Exit::Ok.into())
}

/// `cover … --json`: the effects, the notes and what was done with them.
fn report(
    effects: &Effects,
    notes: &[String],
    committed: Option<&Committed>,
    exit: Exit,
) -> serde_json::Value {
    serde_json::json!({
        "effects": effects,
        "notes": notes,
        "committed": committed.map(|committed| serde_json::json!({
            "txid": committed.txid.as_str(),
            "summary": committed.record.summary,
            "headline": committed.headline(),
        })),
        "exit": exit.code(),
    })
}

/// `cover show`'s text output.
///
/// Tracks whose pictures are identical are printed once as a group — the usual
/// album has fourteen tracks with one cover, and fourteen identical blocks hide
/// the one track that differs.
fn print_survey(survey: &AlbumArt, out: &Out) {
    println!("{}", out.paint(Style::Bold, &format!("{}/", survey.dir)));

    /// Which pictures a track has — kind and hash, in file order.
    type Signature = Vec<(u8, u64)>;
    let mut groups: Vec<(Signature, Vec<&art::read::TrackArt>)> = Vec::new();
    for track in &survey.tracks {
        let key: Vec<(u8, u64)> = track
            .pictures
            .iter()
            .map(|p| (p.kind.code(), p.image.hash))
            .collect();
        match groups.iter_mut().find(|(seen, tracks)| {
            *seen == key && tracks[0].error.is_none() && track.error.is_none()
        }) {
            Some((_, tracks)) => tracks.push(track),
            None => groups.push((key, vec![track])),
        }
    }

    println!();
    println!("Embedded");
    for (_, tracks) in &groups {
        let first = tracks[0];
        let label = if tracks.len() == 1 {
            first.rel.file_name().to_owned()
        } else {
            format!("{} tracks ({} …)", tracks.len(), first.rel.file_name())
        };
        println!("  {}", out.paint(Style::Bold, &label));
        if let Some(error) = &first.error {
            println!("    {} {error}", out.paint(Style::Red, "x"));
        } else if first.pictures.is_empty() {
            println!("    {}", out.paint(Style::Dim, "(no pictures)"));
        }
        for picture in &first.pictures {
            let declared = picture
                .declared
                .as_deref()
                .filter(|mime| *mime != picture.image.format.mime())
                .map(|mime| format!(" (the frame says {mime})"))
                .unwrap_or_default();
            println!(
                "    {:<12} {}{declared}",
                picture.kind.name(),
                picture.image
            );
        }
    }

    println!();
    println!("Loose");
    if survey.loose.is_empty() {
        println!("  {}", out.paint(Style::Dim, "(no image files)"));
    }
    for file in &survey.loose {
        let what = if file.is_image() {
            file.image.to_string()
        } else {
            format!("not an image ({})", human_bytes(file.image.size))
        };
        println!("  {:<14} {what}", file.name);
    }

    if !survey.mismatches.is_empty() {
        println!();
        for mismatch in &survey.mismatches {
            println!("{} {mismatch}", out.paint(Style::Yellow, "!"));
        }
    }
}

/// `+70.0 MB` or `-1.2 MB`.
fn signed_bytes(n: i128) -> String {
    let size = human_bytes(u64::try_from(n.unsigned_abs()).unwrap_or(u64::MAX));
    if n < 0 { format!("-{size}") } else { size }
}

/// `1 file`, `14 files`.
fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}
