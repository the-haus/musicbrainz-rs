//! Builds databases from small synthetic exports, queries and refreshes them.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use musicbrainz_db::{build, refresh, BuildOptions, Database, IndexLevel, OpenOptions};

const BLOCK: usize = 512;

fn checksum(h: &mut [u8; BLOCK]) {
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

fn tar_of(entries: &[(&str, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut h = [0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        checksum(&mut h);
        out.extend_from_slice(&h);
        out.extend_from_slice(data.as_bytes());
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    }
    out.extend_from_slice(&[0u8; BLOCK * 2]);
    out
}

/// An export folder `<root>/<name>/mbdump.tar.bz2` with these tables.
fn export(root: &Path, name: &str, tables: &[(&str, &str)]) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let mut entries = vec![
        ("TIMESTAMP", format!("{name} timestamp\n")),
        ("SCHEMA_SEQUENCE", "31\n".to_string()),
        ("REPLICATION_SEQUENCE", "189799\n".to_string()),
    ];
    for (t, rows) in tables {
        entries.push((Box::leak(format!("mbdump/{t}").into_boxed_str()), rows.to_string()));
    }
    let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(1));
    e.write_all(&tar_of(&entries)).unwrap();
    fs::write(dir.join("mbdump.tar.bz2"), e.finish().unwrap()).unwrap();
    dir
}

const G1: &str = "00000000-0000-0000-0000-000000000001";
const G2: &str = "00000000-0000-0000-0000-000000000002";
const G3: &str = "00000000-0000-0000-0000-000000000003";

fn artist(id: u32, gid: &str, name: &str) -> String {
    format!("{id}\t{gid}\t{name}\t{name}\t1990\t\\N\t\\N\t\\N\t\\N\t\\N\t2\t\\N\t\\N\t\t0\t2026-01-01 00:00:00+00\tf\t\\N\t\\N\n")
}

fn v1() -> Vec<(&'static str, String)> {
    vec![
        ("artist", artist(1, G1, "AC\\\\DC") + &artist(2, G2, "Tab\\tName")),
        ("artist_alias", "10\t1\tACDC\t\\N\t0\t\\N\t1\tACDC\t\\N\t\\N\t\\N\t\\N\t\\N\t\\N\tf\tf\n".to_string()),
        ("link_type", "1\t\\N\t0\t10000000-0000-0000-0000-000000000000\tartist\turl\twikidata\t\twikidata\twikidata page for\thas wikidata page\t\\N\tf\tf\t0\t0\n".to_string()),
        ("link", "100\t1\t\\N\t\\N\t\\N\t\\N\t\\N\t\\N\t0\t\\N\tf\n".to_string()),
        ("url", "5\t20000000-0000-0000-0000-000000000000\thttps://www.wikidata.org/wiki/Q1\t0\t\\N\n".to_string()),
        ("l_artist_url", "50\t100\t1\t5\t0\t\\N\t0\t\t\n".to_string()),
        ("artist_gid_redirect", String::new()),
        ("area", String::new()),
    ]
}

fn opts(source: &Path, db: &Path) -> BuildOptions {
    let mut o = BuildOptions::new(source, db);
    o.threads = 2;
    o.indexes = IndexLevel::Foreign;
    o
}

fn as_refs(v: &[(&'static str, String)]) -> Vec<(&'static str, String)> {
    v.to_vec()
}

#[test]
fn builds_queries_and_refreshes() {
    let root = tempfile::tempdir().unwrap();
    let t1 = as_refs(&v1());
    let t1: Vec<(&str, &str)> = t1.iter().map(|(a, b)| (*a, b.as_str())).collect();
    let src = export(root.path(), "20261014-001756", &t1);
    let db = root.path().join("mb.duckdb");

    let phases = Arc::new(Mutex::new(Vec::new()));
    let mut o = opts(&src, &db);
    let seen = phases.clone();
    o.on_progress = Some(Arc::new(move |p| seen.lock().unwrap().push(p.phase)));
    let stats = build(o).unwrap();
    assert_eq!(stats.tables.iter().filter(|t| t.from != "empty").count(), 8);
    // The core's other tables, left out of the archive as MetaBrainz leaves
    // out empty ones, are there, empty.
    assert!(stats.tables.iter().any(|t| t.name == "l_url_url" && t.from == "empty"));
    assert_eq!(stats.export.name.as_deref(), Some("20261014-001756"));
    assert!(phases.lock().unwrap().contains(&"load"));
    assert!(
        !root.path().join("mb.duckdb.work").exists(),
        "the work folder is removed"
    );
    // A second build refuses to replace it.
    assert!(build(opts(&src, &db)).is_err());

    let d = Database::open(&db, &OpenOptions::default()).unwrap();
    let rows: serde_json::Value = serde_json::from_str(
        &d.query_json("SELECT id, gid, name, ended, last_updated FROM artist ORDER BY id", &[])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rows[0]["name"], "AC\\DC");
    assert_eq!(rows[1]["name"], "Tab\tName");
    assert_eq!(rows[0]["gid"], G1);
    assert_eq!(rows[0]["ended"], false);
    // Arrays bind as JSON text, cast in SQL.
    let n = d
        .query_json(
            "SELECT count(*) AS n FROM artist WHERE gid IN (SELECT unnest(?::UUID[]))",
            &[serde_json::json!([G1, G3])],
        )
        .unwrap();
    assert_eq!(n, "[{\"n\":1}]");
    let rels = d
        .query_json(
            "SELECT link_type, mb_wikidata_id(u.url) AS q FROM mb_relationship r JOIN url u ON u.id = r.entity1 WHERE r.entity0 = ?",
            &[serde_json::json!(1)],
        )
        .unwrap();
    assert_eq!(rels, "[{\"link_type\":\"wikidata\",\"q\":\"Q1\"}]");
    let meta = d.meta().unwrap();
    assert_eq!(meta["schema_sequence"].as_deref(), Some("31"));
    assert_eq!(meta["replication_sequence"].as_deref(), Some("189799"));
    let mut batches = 0;
    d.query_batches("SELECT * FROM range(25)", &[], 10, &mut |b| {
        batches += 1;
        assert!(b.len() <= 10);
        true
    })
    .unwrap();
    assert_eq!(batches, 3);
    assert_eq!(
        d.query_json("SELECT count(*) AS n FROM l_url_url", &[]).unwrap(),
        "[{\"n\":0}]"
    );
    let pragma = d.query_json("SHOW TABLES", &[]).unwrap();
    assert!(pragma.contains("artist_alias"));
    assert!(d.describe("SELECT id FROM artist", &[]).unwrap()[0].0 == "id");
    drop(d);

    // The next export: artist 2 renamed, an alias of 1 changed, a new artist 3,
    // and 2 then merged into 3.
    let mut t2 = v1();
    t2[0].1 = artist(1, G1, "AC\\\\DC") + &artist(3, G3, "New");
    t2[1].1 = "10\t1\tAC/DC\t\\N\t0\t\\N\t1\tAC/DC\t\\N\t\\N\t\\N\t\\N\t\\N\t\\N\tf\tf\n".to_string();
    t2[6].1 = format!("{G2}\t3\t2026-01-02 00:00:00+00\n");
    let t2: Vec<(&str, &str)> = t2.iter().map(|(a, b)| (*a, b.as_str())).collect();
    let src2 = export(root.path(), "20261017-002244", &t2);
    let stats = refresh(opts(&src2, &db), true).unwrap();
    let diff = stats.diff.unwrap();
    assert_eq!(diff.from_export.as_deref(), Some("20261014-001756"));
    let a = &diff.entities["artist"];
    assert_eq!((a.added, a.changed, a.removed, a.merged), (1, 1, 0, 1), "{a:?}");
    assert!(root.path().join("mb.duckdb.previous").exists());

    let d = Database::open(&db, &OpenOptions::default()).unwrap();
    let changes: serde_json::Value = serde_json::from_str(
        &d.query_json(
            "SELECT id, gid, change, tables, merged_into FROM mb_changes WHERE entity = 'artist' ORDER BY id",
            &[],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(changes[0]["change"], "changed");
    assert_eq!(changes[0]["tables"], serde_json::json!(["artist_alias"]));
    assert_eq!(changes[1]["change"], "merged");
    assert_eq!(changes[1]["merged_into"], G3);
    assert_eq!(changes[2]["change"], "added");
    assert_eq!(d.meta().unwrap()["export_name"].as_deref(), Some("20261017-002244"));
}

#[test]
fn only_some_tables_and_wrong_schemas() {
    let root = tempfile::tempdir().unwrap();
    let t1 = v1();
    let t1: Vec<(&str, &str)> = t1.iter().map(|(a, b)| (*a, b.as_str())).collect();
    let src = export(root.path(), "20261014-001756", &t1);
    let db = root.path().join("some.duckdb");
    let mut o = opts(&src, &db);
    o.tables = Some(vec!["artist".into(), "url".into()]);
    let stats = build(o).unwrap();
    let names: Vec<_> = stats.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["artist", "url"]);

    // A table whose columns do not match the schema fails the build, and
    // leaves nothing behind.
    let bad = export(root.path(), "20261017-002244", &[("artist", "1\tonly two\n")]);
    let db2 = root.path().join("bad.duckdb");
    let err = build(opts(&bad, &db2)).unwrap_err().to_string();
    assert!(err.contains("2 fields in the dump, 19 columns"), "{err}");
    assert!(!db2.exists());
    assert!(!root.path().join("bad.duckdb.building").exists());
}
