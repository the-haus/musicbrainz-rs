# Contributing

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude musicbrainz-node
cd crates/bindings/musicbrainz-node && corepack yarn install && corepack yarn build && corepack yarn test
```

- The tests build databases from `fixtures/daft-punk`: a consistent subset
  of the 2026-10-17 core export (CC0) around one artist, made by
  `musicbrainz subset <db> fixtures/daft-punk --seed 056e4f3e-d505-4dad-8ec1-d04f521cbb56`.
  Regenerate it rather than editing it by hand, and update the tests'
  expectations. Archives (`.tar.bz2`) are made by the tests themselves.
- The schema is musicbrainz-server's SQL (`crates/musicbrainz-schema/sql`):
  for a new schema sequence, replace those files and `src/dumps.rs` (from
  `Constants.pm`), bump `SCHEMA_SEQUENCE`, and check a real export's column
  counts (`cargo run -p musicbrainz-schema --example columns`).
- The bzip2 decoder is wikidata-rs's (`bz2.rs`, `bzdecode.rs`, `parallel.rs`
  in `musicbrainz-dump`); fixes belong there first.
- Behaviour that follows the dumps' or MusicBrainz's conventions should say
  so in a comment, with the reason when it is surprising.
- Commits follow Conventional Commits (`feat:`, `fix:`, `perf:`, …).

## Releasing

See [RELEASING.md](RELEASING.md).
