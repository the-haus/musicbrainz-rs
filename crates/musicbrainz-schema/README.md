# musicbrainz-schema

The MusicBrainz database schema as data: every table of a full export
(`mbdump.tar.bz2` and the other archives), its columns in the order of the
dump's fields, their types, which may be `NULL`, the primary key and the
foreign keys. It is parsed from musicbrainz-server's own `CreateTables.sql`,
`CreatePrimaryKeys.sql` and `CreateFKConstraints.sql` (bundled, schema
sequence 31), so nothing is typed in by hand.

```rust
use musicbrainz_schema::{Kind, Schema};

let schema = Schema::bundled();
let artist = schema.table("artist").unwrap();
assert_eq!(artist.columns[1].name, "gid");
assert_eq!(artist.columns[1].kind, Kind::Uuid);
assert_eq!(artist.foreign_key("area").unwrap().table, "area");
assert_eq!(artist.dump_file().unwrap().file, "mbdump.tar.bz2");
```

A dump of another schema sequence is read with `Schema::from_sql` and that
release's SQL files.
