#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

mod dumps;
mod parse;

use std::collections::HashMap;
use std::sync::OnceLock;

pub use dumps::{DumpFile, DUMP_FILES};
pub use parse::ParseError;

/// The schema sequence the bundled SQL is (`DB_SCHEMA_SEQUENCE` of
/// musicbrainz-server; the dumps' `SCHEMA_SEQUENCE` file).
pub const SCHEMA_SEQUENCE: u32 = 31;

/// The bundled `CreateTables.sql`, `CreatePrimaryKeys.sql` and
/// `CreateFKConstraints.sql` of each PostgreSQL schema, as
/// `(schema, tables, primary keys, foreign keys)`.
pub const BUNDLED_SQL: &[(&str, &str, &str, &str)] = &[
    (
        "musicbrainz",
        include_str!("../sql/musicbrainz/CreateTables.sql"),
        include_str!("../sql/musicbrainz/CreatePrimaryKeys.sql"),
        include_str!("../sql/musicbrainz/CreateFKConstraints.sql"),
    ),
    (
        "cover_art_archive",
        include_str!("../sql/caa/CreateTables.sql"),
        include_str!("../sql/caa/CreatePrimaryKeys.sql"),
        include_str!("../sql/caa/CreateFKConstraints.sql"),
    ),
    (
        "event_art_archive",
        include_str!("../sql/eaa/CreateTables.sql"),
        include_str!("../sql/eaa/CreatePrimaryKeys.sql"),
        include_str!("../sql/eaa/CreateFKConstraints.sql"),
    ),
    (
        "documentation",
        include_str!("../sql/documentation/CreateTables.sql"),
        include_str!("../sql/documentation/CreatePrimaryKeys.sql"),
        include_str!("../sql/documentation/CreateFKConstraints.sql"),
    ),
    (
        "statistics",
        include_str!("../sql/statistics/CreateTables.sql"),
        include_str!("../sql/statistics/CreatePrimaryKeys.sql"),
        "",
    ),
    (
        "wikidocs",
        include_str!("../sql/wikidocs/CreateTables.sql"),
        include_str!("../sql/wikidocs/CreatePrimaryKeys.sql"),
        "",
    ),
];

/// Views that are dumped like tables, and the table whose columns they have
/// (`CreateViews.sql`: `editor_sanitized` is `sanitize_editor(editor)`).
const VIEWS: &[(&str, &str)] = &[("editor_sanitized", "editor")];

/// What a column holds, as far as reading it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `SMALLINT`, `INTEGER`, `SERIAL`, `BIGINT`: a number.
    Integer,
    /// `REAL`, `DOUBLE PRECISION`, `NUMERIC`: a number.
    Float,
    /// `BOOLEAN`: `t` or `f`.
    Boolean,
    /// `UUID`: an MBID.
    Uuid,
    /// `TEXT`, `VARCHAR`, `CHAR` and enumerated types.
    Text,
    /// `TIMESTAMP` (with or without time zone).
    Timestamp,
    /// `DATE`.
    Date,
    /// `TIME`.
    Time,
    /// `INTERVAL`.
    Interval,
    /// `POINT`: `(x,y)`.
    Point,
    /// `JSON`, `JSONB`.
    Json,
    /// An array of integers (`INTEGER[]`, `SMALLINT[]`).
    IntegerArray,
    /// An array of text (`TEXT[]`, `VARCHAR[]`).
    TextArray,
    /// Anything else (`CUBE`), read as text.
    Other,
}

impl Kind {
    /// The kind of an SQL type as `CreateTables.sql` spells it (`VARCHAR(255)`,
    /// `TIMESTAMP WITH TIME ZONE`, `INTEGER[]`); `None` for no type at all.
    pub fn of_sql(sql: &str, enums: &[String]) -> Option<Kind> {
        let upper = sql.to_ascii_uppercase();
        let array = upper.ends_with("[]");
        let base = upper.trim_end_matches("[]");
        let word = base.split(['(', ' ']).next().unwrap_or("");
        let kind = match word {
            "SMALLINT" | "INTEGER" | "INT" | "INT4" | "INT2" | "INT8" | "BIGINT" | "SERIAL" | "BIGSERIAL" => {
                Kind::Integer
            }
            "REAL" | "DOUBLE" | "NUMERIC" | "DECIMAL" | "FLOAT" => Kind::Float,
            "BOOLEAN" | "BOOL" => Kind::Boolean,
            "UUID" => Kind::Uuid,
            "TEXT" | "VARCHAR" | "CHAR" | "CHARACTER" | "CITEXT" => Kind::Text,
            "TIMESTAMP" | "TIMESTAMPTZ" => Kind::Timestamp,
            "DATE" => Kind::Date,
            "TIME" => Kind::Time,
            "INTERVAL" => Kind::Interval,
            "POINT" => Kind::Point,
            "JSON" | "JSONB" => Kind::Json,
            "CUBE" | "BYTEA" | "INET" | "TSVECTOR" => Kind::Other,
            _ if enums.iter().any(|e| e.eq_ignore_ascii_case(word)) => Kind::Text,
            // An enumerated type or domain defined elsewhere (`CreateTypes.sql`).
            w if !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') => Kind::Text,
            _ => return None,
        };
        Some(match (array, kind) {
            (false, k) => k,
            (true, Kind::Integer) => Kind::IntegerArray,
            (true, _) => Kind::TextArray,
        })
    }

    /// Its name in lower case (`"integer"`, `"uuid"`, `"integerArray"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Integer => "integer",
            Kind::Float => "float",
            Kind::Boolean => "boolean",
            Kind::Uuid => "uuid",
            Kind::Text => "text",
            Kind::Timestamp => "timestamp",
            Kind::Date => "date",
            Kind::Time => "time",
            Kind::Interval => "interval",
            Kind::Point => "point",
            Kind::Json => "json",
            Kind::IntegerArray => "integerArray",
            Kind::TextArray => "textArray",
            Kind::Other => "other",
        }
    }
}

/// A column of a table, in the dump's order.
#[derive(Clone, Debug)]
pub struct Column {
    /// Its name.
    pub name: String,
    /// Its SQL type as written (`VARCHAR(255)`).
    pub sql_type: String,
    /// What it holds.
    pub kind: Kind,
    /// Whether it may be `NULL` (`\N` in the dump).
    pub nullable: bool,
}

/// A foreign key: `column` holds an id of `table.column`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKey {
    /// The referring column (one column; MusicBrainz has no wider ones).
    pub column: String,
    /// The referenced table (qualified when not in `musicbrainz`).
    pub table: String,
    /// The referenced column (`id`, almost always).
    pub references: String,
}

/// One table: its columns in the dump's order, its primary key and its
/// foreign keys.
#[derive(Clone, Debug)]
pub struct Table {
    /// Its name as it is in the dumps: `artist`, `cover_art_archive.art_type`.
    pub name: String,
    /// Its PostgreSQL schema (`musicbrainz`, `cover_art_archive`, …).
    pub schema: String,
    /// Its columns, in the order of the dump's fields.
    pub columns: Vec<Column>,
    /// Its primary key's columns.
    pub primary_key: Vec<String>,
    /// Its foreign keys.
    pub foreign_keys: Vec<ForeignKey>,
}

impl Table {
    /// The index of a column, by name.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// A column, by name.
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// The foreign key on a column, if any.
    pub fn foreign_key(&self, column: &str) -> Option<&ForeignKey> {
        self.foreign_keys.iter().find(|f| f.column == column)
    }

    /// Whether its primary key is one integer column (`id`, mostly).
    pub fn integer_key(&self) -> Option<&str> {
        match self.primary_key.as_slice() {
            [k] if self.column(k).is_some_and(|c| c.kind == Kind::Integer) => Some(k),
            _ => None,
        }
    }

    /// The archive of a full export it is in (by [`DUMP_FILES`]).
    pub fn dump_file(&self) -> Option<&'static DumpFile> {
        DUMP_FILES.iter().find(|d| d.tables.contains(&self.name.as_str()))
    }

    /// For a relationship table (`l_<entity0>_<entity1>`), its two entity
    /// types: `l_release_group_url` is `("release_group", "url")`.
    pub fn link_entities(&self) -> Option<(&'static str, &'static str)> {
        link_entities(&self.name)
    }
}

/// The entity types relationships join (`l_<a>_<b>`, `a <= b`).
pub const ENTITY_TYPES: &[&str] = &[
    "area",
    "artist",
    "event",
    "genre",
    "instrument",
    "label",
    "place",
    "recording",
    "release",
    "release_group",
    "series",
    "url",
    "work",
];

/// The two entity types of a relationship table: `l_area_release_group` is
/// `("area", "release_group")`; `None` for any other name.
pub fn link_entities(table: &str) -> Option<(&'static str, &'static str)> {
    let rest = table.strip_prefix("l_")?;
    for a in ENTITY_TYPES {
        if let Some(b) = rest.strip_prefix(a).and_then(|r| r.strip_prefix('_')) {
            if let Some(b) = ENTITY_TYPES.iter().find(|t| **t == b) {
                return Some((a, b));
            }
        }
    }
    None
}

/// The relationship table between two entity types (in either order):
/// `("url", "artist")` is `l_artist_url`.
pub fn link_table(a: &str, b: &str) -> String {
    if a <= b {
        format!("l_{a}_{b}")
    } else {
        format!("l_{b}_{a}")
    }
}

/// Every table of a schema, by name.
#[derive(Clone, Debug)]
pub struct Schema {
    tables: Vec<Table>,
    by_name: HashMap<String, usize>,
    /// Its schema sequence.
    pub sequence: u32,
}

impl Schema {
    /// The schema bundled with this crate ([`SCHEMA_SEQUENCE`]), parsed once.
    pub fn bundled() -> &'static Schema {
        static SCHEMA: OnceLock<Schema> = OnceLock::new();
        SCHEMA.get_or_init(|| {
            Schema::from_sql(BUNDLED_SQL.iter().copied(), SCHEMA_SEQUENCE).expect("the bundled schema parses")
        })
    }

    /// A schema from SQL files, `(schema, CreateTables, CreatePrimaryKeys,
    /// CreateFKConstraints)` each (the latter two may be empty). Tables
    /// outside `musicbrainz` are named `<schema>.<table>`, as in the dumps.
    pub fn from_sql<'a>(
        files: impl IntoIterator<Item = (&'a str, &'a str, &'a str, &'a str)>,
        sequence: u32,
    ) -> Result<Schema, ParseError> {
        let mut tables = Vec::new();
        let mut keys = Vec::new();
        let mut fks = Vec::new();
        for (schema, create, pks, fk) in files {
            let qualify = |t: &str| parse::qualified(schema, t);
            for t in parse::tables(schema, create)? {
                tables.push(t);
            }
            for (t, cols) in parse::primary_keys(pks) {
                keys.push((qualify(&t), cols));
            }
            for (t, f) in parse::foreign_keys(fk) {
                fks.push((qualify(&t), f));
            }
        }
        // Views dumped as tables, with the columns of the table they show.
        for (view, of) in VIEWS {
            if !tables.iter().any(|t| t.name == *view) {
                if let Some(t) = tables.iter().find(|t| t.name == *of) {
                    let mut v = t.clone();
                    v.name = view.to_string();
                    tables.push(v);
                }
            }
        }
        let mut by_name = HashMap::new();
        for (i, t) in tables.iter().enumerate() {
            by_name.insert(t.name.clone(), i);
        }
        for (t, cols) in keys {
            if let Some(&i) = by_name.get(&t) {
                tables[i].primary_key = cols;
            }
        }
        for (t, f) in fks {
            if let Some(&i) = by_name.get(&t) {
                if !tables[i].foreign_keys.contains(&f) {
                    tables[i].foreign_keys.push(f);
                }
            }
        }
        Ok(Schema {
            tables,
            by_name,
            sequence,
        })
    }

    /// A table, by its name in the dumps.
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.by_name.get(name).map(|&i| &self.tables[i])
    }

    /// Every table, in the SQL files' order.
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    /// The relationship tables an entity type takes part in, with the side
    /// it is on: `(table, true)` where it is `entity0`, `(table, false)`
    /// where it is `entity1` (a table of one type to itself is listed with
    /// both).
    pub fn link_tables_of(&self, entity: &str) -> Vec<(&Table, bool)> {
        let mut out = Vec::new();
        for t in &self.tables {
            if let Some((a, b)) = t.link_entities() {
                if a == entity {
                    out.push((t, true));
                }
                if b == entity {
                    out.push((t, false));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_bundled_schema() {
        let s = Schema::bundled();
        let artist = s.table("artist").unwrap();
        let names: Vec<_> = artist.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names.join(" "),
            "id gid name sort_name begin_date_year begin_date_month begin_date_day end_date_year end_date_month \
             end_date_day type area gender comment edits_pending last_updated ended begin_area end_area"
        );
        assert_eq!(artist.primary_key, vec!["id"]);
        assert_eq!(artist.column("gid").unwrap().kind, Kind::Uuid);
        assert_eq!(artist.column("ended").unwrap().kind, Kind::Boolean);
        assert!(!artist.column("name").unwrap().nullable);
        assert!(artist.column("type").unwrap().nullable);
        let fk = artist.foreign_key("area").unwrap();
        assert_eq!((fk.table.as_str(), fk.references.as_str()), ("area", "id"));
        assert_eq!(artist.dump_file().unwrap().file, "mbdump.tar.bz2");
        assert_eq!(artist.integer_key(), Some("id"));
    }

    #[test]
    fn every_dumped_table_is_in_the_schema() {
        let s = Schema::bundled();
        let missing: Vec<_> = DUMP_FILES
            .iter()
            .flat_map(|d| d.tables.iter())
            .filter(|t| s.table(t).is_none())
            .collect();
        assert!(missing.is_empty(), "not in the schema: {missing:?}");
        for d in DUMP_FILES {
            for t in d.tables {
                let t = s.table(t).unwrap();
                assert!(!t.columns.is_empty(), "{} has no columns", t.name);
            }
        }
    }

    #[test]
    fn composite_and_qualified_keys() {
        let s = Schema::bundled();
        assert_eq!(
            s.table("artist_credit_name").unwrap().primary_key,
            vec!["artist_credit", "position"]
        );
        let caa = s.table("cover_art_archive.cover_art").unwrap();
        assert_eq!(caa.schema, "cover_art_archive");
        assert_eq!(caa.foreign_key("release").unwrap().table, "release");
        let place = s.table("place").unwrap();
        assert_eq!(place.column("coordinates").unwrap().kind, Kind::Point);
    }

    #[test]
    fn link_tables() {
        assert_eq!(link_entities("l_release_group_url"), Some(("release_group", "url")));
        assert_eq!(link_entities("l_area_release_group"), Some(("area", "release_group")));
        assert_eq!(link_entities("l_artist_artist"), Some(("artist", "artist")));
        assert_eq!(link_entities("link_type"), None);
        assert_eq!(link_table("url", "artist"), "l_artist_url");
        let s = Schema::bundled();
        let artist = s.link_tables_of("artist");
        assert_eq!(artist.len(), ENTITY_TYPES.len() + 1);
        let l = s.table("l_artist_url").unwrap();
        assert_eq!(l.foreign_key("entity1").unwrap().table, "url");
    }
}
