# Releasing

One `vX.Y.Z` tag on `main` releases everything
([`release.yml`](.github/workflows/release.yml)): `@musicbrainz-rs/node` and
its seven platform packages on npm, the `musicbrainz` CLI for five targets,
and a GitHub release with the notes, the archives and `SHA256SUMS`.

## Cutting one

1. Set the version everywhere (the Cargo workspace, the pinned internal
   crate dependencies, the Node package, `Cargo.lock`) and open a
   `## X.Y.Z` section in `CHANGELOG.md`:

   ```sh
   node scripts/bump-version.mjs X.Y.Z
   ```

2. Fill in that section (it becomes the release notes) and replace
   `unreleased` with the date.
3. Check that everything agrees (CI runs this on every push), commit and
   push:

   ```sh
   node scripts/check-version.mjs vX.Y.Z
   git commit -am "chore: release X.Y.Z" && git push
   ```

4. When `ci` and `node` pass on `main`, push the tag:

   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```

Prereleases such as `X.Y.Z-rc.1` go to npm's `next` dist-tag and are marked
as pre-releases on GitHub.

## Dry runs

Run **release** by hand from the Actions tab with `dry-run` on (the
default): it builds and tests every binary, packs the npm tarballs and
installs them on every platform, but publishes nothing.

## What the pipeline does

The workflow checks the versions against the tag, runs fmt, clippy and the
tests in release mode, and then, in parallel:

- publishes `@musicbrainz-rs/node` and its platform packages
  ([`release-node.yml`](.github/workflows/release-node.yml)): each binary is
  built and tested on its own platform (Linux in Node's Debian bullseye and
  Alpine images: glibc 2.31+ and musl), every platform package is checked
  for its binary and repository, and the packed tarballs are installed and
  used on every platform with Node 18 before anything is published; versions
  already on npm are skipped, so a failed release can be re-run;
- builds `musicbrainz` for Linux (x64, arm64), macOS (arm64, x64) and
  Windows (x64), with a smoke test that builds and queries the fixture
  export;

and finally creates the GitHub release.

DuckDB is compiled from source into every binary (the `duckdb` crate's
`bundled` feature): a cold build takes several minutes, which is why the
jobs' timeouts are generous.

The crates are not published to crates.io yet; `cargo package` in CI keeps
them publishable.

## Settings and secrets

- `NPM_TOKEN`: an npm automation token with publish rights on the
  `@musicbrainz-rs` scope (or npm trusted publishing for this workflow).
- The `npm-publish` environment: add required reviewers to gate publishing.
- npm provenance is added when the repository is public (and needs
  `repository.url` in the package to point at it).

## Publishing by hand

When Actions cannot run (no runners, no quota), the same packages can be
published from a machine with every target's binary, or with only the
current platform's for a first release:

```sh
cd crates/bindings/musicbrainz-node
corepack yarn build && corepack yarn copy-license
corepack yarn exec napi create-npm-dir -t .
corepack yarn exec napi artifacts --dir artifacts --dist npm   # binaries in artifacts/
# then, as release-node.yml does: npm publish each npm/<triple> package, then the root
```
