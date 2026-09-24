# 32 — Packaging and release

- **Phase:** M6 · Polish
- **Depends on:** 31
- **Status:** not started

## Goal

A version the user can install on their Arch machine and update without building
from a checkout by hand.

## Details

**Release profile** in `Cargo.toml`: `lto = "thin"`, `codegen-units = 1`,
`strip = true`, `panic = "abort"` — but verify `panic = "abort"` does not break
the panic-hook terminal restore from task 20; if it does, keep unwinding. Record
the binary size and startup time.

**CI** (GitHub Actions, if the repo gets a remote): `fmt --check`, `clippy -D
warnings`, `test` on stable, and a release job building a tagged tarball. The
test suite must not require a running MPD daemon — task 13's transcript tests
exist for this.

**Arch packaging**: a `PKGBUILD` (`mpdfm-git`, or `mpdfm` from a tagged tarball)
installing the binary, the man page, completions and the example configs. Test it
with `makepkg -si` in a clean chroot if convenient, or at least `namcap` it.

**Versioning**: semver, `0.x` while the format of the journal can still change.
The journal `version` field (task 11) is the compatibility contract — bump it and
handle old records, or refuse them with a clear message.

**Release checklist** in `docs/RELEASING.md`:
1. `just check` green
2. full test suite including the at-scale organize test
3. manual smoke test on a **copy** of the real library: doctor, move, undo,
   tag edit, organize --dry-run
4. CHANGELOG updated, version bumped, tag pushed
5. PKGBUILD `pkgver`/`sha256sums` updated

## Acceptance criteria

- [ ] `cargo build --release` produces a working binary; size and startup time
      recorded in this file
- [ ] panic-hook terminal restore still works under the release profile
- [ ] CI runs fmt, clippy and tests with no MPD daemon available
- [ ] `PKGBUILD` builds and installs binary + man page + completions + examples
- [ ] the installed binary runs and finds the user's mpd.conf
- [ ] `mpdfm --version` reports the crate version and the git hash
- [ ] a journal record from an older `version` is either migrated or refused with
      a clear message (test with a hand-written old record)
- [ ] `docs/RELEASING.md` exists and was followed for the first tag

## Files

`Cargo.toml`, `.github/workflows/ci.yml`, `packaging/PKGBUILD`,
`docs/RELEASING.md`

## Pitfalls

- `panic = "abort"` skips `Drop`, which would defeat `TerminalGuard`. The panic
  hook runs first, so it can still restore the terminal — but test it explicitly
  rather than assuming.
- Don't ship a 1.0 while the journal format can still change; undo across an
  upgrade is the thing users will trust and be hurt by.
