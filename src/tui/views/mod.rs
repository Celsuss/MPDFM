//! The views: the state behind each screen, as distinct from the shell that
//! stacks them and the widgets that draw them.
//!
//! | module | owns |
//! | --- | --- |
//! | `browser` | the library browser — where you are, what is marked, what is sorted how |
//! | `tagedit` | the tag editor — the selection, what was typed into it, what that would write |
//! | `pending` | the staged plan — what is about to happen, folded and unfolded |
//! | `organize` | the template being typed, and where the first files would go |
//! | `search` | the pattern being typed, and a library-wide walk in flight |
//! | `error` | something that went wrong, the path it is about, and what to do next |
//!
//! The shape they all follow is the one `browser` sets:
//!
//! - **the view owns its own cursor.** Not [`App`][crate::tui::app::App], which is
//!   why pushing an overlay and popping it again cannot lose a position;
//! - **the view holds no `Library`.** It is handed one for the duration of a call
//!   and borrows nothing across a frame, so a rescan replaces the model without
//!   the view noticing (`docs/tasks/20-tui-shell.md`'s second pitfall);
//! - **rendering is a pure function of the view plus the model.** Everything that
//!   mutates — the cursor, the scroll offset, the tag requests — happens in
//!   `update`, never in `render`.

pub mod browser;
pub mod error;
pub mod organize;
pub mod pending;
pub mod search;
pub mod tagedit;
