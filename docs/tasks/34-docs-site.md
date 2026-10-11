# 34 — Documentation site

- **Phase:** M6 · Polish
- **Depends on:** 31
- **Status:** built; needs repo Settings → Pages → Source = "GitHub Actions" and a first deploy

## Goal

Host the documentation as a searchable site on GitHub Pages
(`https://celsuss.github.io/MPDFM/`), without a second copy of anything that
already lives in `docs/`.

## What was built

- **mdBook** in `book/` (`book.toml`, `src/`). New pages: introduction, install,
  first run, a guide page per command family, the TUI, file locations and exit
  codes.
- **Included, not copied:** `docs/SAFETY.md`, `PLAN.md`, `config.example.toml`,
  `keys.example.toml` and `CHANGELOG.md` via `{{#include}}`.
- **Generated, git-ignored:** `scripts/gen-cli-reference.sh` writes one CLI
  reference page per command from `mpdfm … --help`, and a copy of `ROADMAP.md`
  whose `tasks/…` links point at GitHub (the site does not carry the task files).
- `just book` / `just book-serve`.
- `.github/workflows/docs.yml`: PRs build and link-check; pushes to `main`
  deploy through `actions/deploy-pages`.

## Acceptance criteria

- [x] `just book` builds with no warnings
- [x] `lychee --offline` finds no dead links in the built site
- [ ] first deploy to `main` succeeds and the site loads
