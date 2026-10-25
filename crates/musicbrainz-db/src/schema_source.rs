//! Which schema a dump is read with: the one bundled with this crate when
//! the dump's `SCHEMA_SEQUENCE` is that one's, SQL files given by the caller,
//! or musicbrainz-server's SQL of that sequence, fetched once from GitHub
//! and cached. So a dump of a newer schema loads without a new release of
//! this crate.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use musicbrainz_schema::{Schema, SCHEMA_SEQUENCE};

use crate::{Error, Result};

/// The PostgreSQL schemas a full export has tables of, and where their SQL
/// is in musicbrainz-server (`admin/sql/<dir>`; the core is `admin/sql`).
const SQL_DIRS: &[(&str, &str)] = &[
    ("musicbrainz", ""),
    ("cover_art_archive", "caa"),
    ("event_art_archive", "eaa"),
    ("documentation", "documentation"),
    ("statistics", "statistics"),
    ("wikidocs", "wikidocs"),
];

const SQL_FILES: &[&str] = &["CreateTables.sql", "CreatePrimaryKeys.sql", "CreateFKConstraints.sql"];

const SERVER_RAW: &str = "https://raw.githubusercontent.com/metabrainz/musicbrainz-server";

/// Where a schema came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaOrigin {
    /// Bundled with this crate.
    Bundled,
    /// SQL files in a folder (given, or cached from a fetch).
    Files(PathBuf),
}

/// Reads a folder of SQL files: musicbrainz-server's `admin/sql` layout
/// (`CreateTables.sql` at the top, `caa/`, `eaa/`, … beside it) or this
/// crate's (`musicbrainz/`, `caa/`, …).
pub fn schema_from_dir(dir: &Path, sequence: u32) -> Result<Schema> {
    let mut files: Vec<(String, String, String, String)> = Vec::new();
    for (schema, sub) in SQL_DIRS {
        let candidates = if sub.is_empty() {
            vec![dir.to_path_buf(), dir.join("musicbrainz")]
        } else {
            vec![dir.join(sub)]
        };
        let Some(d) = candidates.into_iter().find(|d| d.join("CreateTables.sql").is_file()) else {
            continue;
        };
        let read = |f: &str| fs::read_to_string(d.join(f)).unwrap_or_default();
        files.push((
            schema.to_string(),
            read(SQL_FILES[0]),
            read(SQL_FILES[1]),
            read(SQL_FILES[2]),
        ));
    }
    if !files.iter().any(|f| f.0 == "musicbrainz") {
        return Err(Error::Schema(format!("{}: no CreateTables.sql", dir.display())));
    }
    Schema::from_sql(
        files
            .iter()
            .map(|(s, a, b, c)| (s.as_str(), a.as_str(), b.as_str(), c.as_str())),
        sequence,
    )
    .map_err(|e| Error::Schema(e.to_string()))
}

/// The schema to read a dump of `sequence` with (see the module docs):
/// `dir` when given; the bundled one for its sequence (or an unknown one);
/// else `<cache>/schema-<sequence>`, fetched there first when missing
/// (unless `offline`).
pub fn resolve_schema(
    sequence: Option<u32>,
    dir: Option<&Path>,
    cache: Option<&Path>,
    offline: bool,
) -> Result<(Schema, SchemaOrigin)> {
    if let Some(dir) = dir {
        let schema = schema_from_dir(dir, sequence.unwrap_or(SCHEMA_SEQUENCE))?;
        return Ok((schema, SchemaOrigin::Files(dir.to_path_buf())));
    }
    let seq = match sequence {
        None => return Ok((Schema::bundled().clone(), SchemaOrigin::Bundled)),
        Some(s) if s == SCHEMA_SEQUENCE => return Ok((Schema::bundled().clone(), SchemaOrigin::Bundled)),
        Some(s) => s,
    };
    let Some(cache) = cache else {
        return Err(Error::Schema(format!(
            "the dump is of schema sequence {seq}, this build bundles {SCHEMA_SEQUENCE}: give the SQL of \
             sequence {seq} (musicbrainz-server's admin/sql) or a cache folder to fetch it into"
        )));
    };
    let dir = cache.join(format!("schema-{seq}"));
    if !dir.join("musicbrainz").join("CreateTables.sql").is_file() {
        if offline {
            return Err(Error::Schema(format!(
                "no SQL of schema sequence {seq} in {} (offline)",
                dir.display()
            )));
        }
        fetch_schema(seq, &dir)?;
    }
    let schema = schema_from_dir(&dir, seq)?;
    Ok((schema, SchemaOrigin::Files(dir)))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(musicbrainz_dump::USER_AGENT)
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Option<String>> {
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| Error::Schema(format!("{url}: {e}")))?;
    match resp.status().as_u16() {
        200 => {
            let (_, body) = resp.into_parts();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut body.into_reader(), &mut s)?;
            Ok(Some(s))
        }
        404 => Ok(None),
        s => Err(Error::Schema(format!("{url}: HTTP status {s}"))),
    }
}

/// `sub DB_SCHEMA_SEQUENCE { 31 }` in `lib/DBDefs/Default.pm`.
pub fn sequence_in_dbdefs(text: &str) -> Option<u32> {
    let at = text.find("sub DB_SCHEMA_SEQUENCE")?;
    let rest = &text[at..];
    let open = rest.find('{')?;
    let close = rest.find('}')?;
    rest.get(open + 1..close)?.trim().parse().ok()
}

/// The git refs of musicbrainz-server that may have the SQL of a schema:
/// its branches, then its schema-change release tags, newest first.
fn candidate_refs(agent: &ureq::Agent) -> Vec<String> {
    let mut refs: Vec<String> = ["production", "master", "beta"].iter().map(|s| s.to_string()).collect();
    if let Ok(Some(json)) = get(
        agent,
        "https://api.github.com/repos/metabrainz/musicbrainz-server/tags?per_page=100",
    ) {
        if let Ok(serde_json::Value::Array(tags)) = serde_json::from_str::<serde_json::Value>(&json) {
            let mut names: Vec<String> = tags
                .iter()
                .filter_map(|t| t.get("name")?.as_str())
                .filter(|n| n.contains("schema-change"))
                .map(str::to_string)
                .collect();
            names.sort();
            names.reverse();
            refs.extend(names);
        }
    }
    refs
}

/// Fetches musicbrainz-server's SQL of schema `sequence` into `dir` (beside,
/// then renamed into place).
pub fn fetch_schema(sequence: u32, dir: &Path) -> Result<()> {
    let agent = agent();
    for r in candidate_refs(&agent) {
        let Some(defs) = get(&agent, &format!("{SERVER_RAW}/{r}/lib/DBDefs/Default.pm"))? else {
            continue;
        };
        if sequence_in_dbdefs(&defs) != Some(sequence) {
            continue;
        }
        let partial = dir.with_extension("partial");
        let _ = fs::remove_dir_all(&partial);
        for (schema, sub) in SQL_DIRS {
            let local = partial.join(if sub.is_empty() { "musicbrainz" } else { sub });
            fs::create_dir_all(&local)?;
            for f in SQL_FILES {
                let path = if sub.is_empty() {
                    format!("admin/sql/{f}")
                } else {
                    format!("admin/sql/{sub}/{f}")
                };
                match get(&agent, &format!("{SERVER_RAW}/{r}/{path}"))? {
                    Some(text) => fs::write(local.join(f), text)?,
                    None if *schema == "musicbrainz" => {
                        return Err(Error::Schema(format!("{r}: no {path}")));
                    }
                    None => {}
                }
            }
        }
        fs::write(partial.join("REF"), format!("{r}\n"))?;
        let _ = fs::remove_dir_all(dir);
        if let Some(p) = dir.parent() {
            fs::create_dir_all(p)?;
        }
        fs::rename(&partial, dir)?;
        return Ok(());
    }
    Err(Error::Schema(format!(
        "found no musicbrainz-server release of schema sequence {sequence}; give its admin/sql folder"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_sequence() {
        let text = "sub DB_SCHEMA_SEQUENCE { 31 }\n";
        assert_eq!(sequence_in_dbdefs(text), Some(31));
        assert_eq!(sequence_in_dbdefs("nothing"), None);
    }

    #[test]
    fn bundled_and_folders() {
        let (s, o) = resolve_schema(Some(SCHEMA_SEQUENCE), None, None, true).unwrap();
        assert_eq!(o, SchemaOrigin::Bundled);
        assert!(s.table("artist").is_some());
        assert!(resolve_schema(Some(99), None, None, true).is_err());
        let sql = concat!(env!("CARGO_MANIFEST_DIR"), "/../musicbrainz-schema/sql");
        let (s, _) = resolve_schema(Some(31), Some(Path::new(sql)), None, true).unwrap();
        assert!(s.table("cover_art_archive.art_type").is_some());
        let cache = tempfile::tempdir().unwrap();
        assert!(resolve_schema(Some(99), None, Some(cache.path()), true).is_err());
    }
}
