//! Organize by template: turning tags into destination paths (task 27).
//!
//! Three layers, each usable on its own:
//!
//! - [`template`] parses `{genre}/{albumartist}/{year} - {album}/{track:02} {title}`
//!   and renders it against one file's tags.
//! - [`sanitize`] makes each rendered segment a name every filesystem the
//!   library may be copied to will take, within the 255-byte limit.
//! - [`plan`] maps a whole selection, deciding what travels with each album,
//!   what collides, and what stays where it is.
//!
//! Nothing here touches the disk. The output is a [`Mapping`]; task 28 turns it
//! into a [`Plan`][crate::ops::Plan] and the ordinary validate → preview →
//! commit pipeline does the rest.
//!
//! **No metadata is ever invented.** A file the template cannot place stays
//! where it is and is reported, rather than being filed under
//! `Unknown Artist/Unknown Album`. The template's `{tag|default}` form is the
//! explicit opt-in for a fallback.
//!
//! ```
//! use std::collections::BTreeMap;
//!
//! use mpdfm_core::library::Library;
//! use mpdfm_core::organize::{self, Options, Template};
//! use mpdfm_core::tags::{TagSet, Values};
//! use mpdfm_core::testing::{names, Fixture};
//!
//! let fixture = Fixture::realistic();
//! let library = Library::scan(fixture.music_dir())?;
//! let template = Template::parse("{albumartist}/{year} - {album}/{track:02} {title}")?;
//!
//! let track = fixture.rel(names::SNOOP_TRACK);
//! let tags = TagSet {
//!     album_artist: Values::one("Snoop Dogg & Wiz Khalifa"),
//!     album: Values::one("Mac + Devin Go To High School"),
//!     date: Values::one("2011"),
//!     track: Some((1, Some(18))),
//!     title: Values::one("Smokin' On"),
//!     ..TagSet::default()
//! };
//! let mapping = organize::map(&template, &library, &BTreeMap::from([(track.clone(), tags)]),
//!                             &Options::default());
//!
//! assert_eq!(
//!     mapping.destination(&track).map(|p| p.as_str()),
//!     Some("Snoop Dogg & Wiz Khalifa/2011 - Mac + Devin Go To High School/01 Smokin' On.mp3"),
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod plan;
pub mod sanitize;
pub mod template;

pub use plan::{Conflict, Mapping, Move, Options, SplitAux, Warning, map};
pub use sanitize::NameRules;
pub use template::{RenderContext, Template, TemplateError, Token, Unplaceable};
