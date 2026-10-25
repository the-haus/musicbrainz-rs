# musicbrainz-dump

Reading MusicBrainz full exports (`mbdump.tar.bz2` and the other archives)
without PostgreSQL:

- **Parallel bzip2.** The block decoder of wikidata-rs (vendored), which
  finds block boundaries at any bit and decodes them on every core, about
  twice as fast as libbzip2 per core.
- **Streaming tar.** Members are read straight out of the decompressor;
  `Archive::extract` writes the tables asked for (each beside and renamed,
  so an interrupted pass leaves nothing that looks whole) and calls back as
  each one is in place, so a loader can start on it while the rest is still
  being decompressed.
- **COPY text.** `copy::fields`, `copy::text` and `copy::unescape` read
  PostgreSQL's `COPY TO` format the tables are in.
- **Downloads.** `download::latest_export`, `download::download_export`:
  resumable, hashed as they arrive, renamed only once the SHA-256 matches
  `SHA256SUMS`.
