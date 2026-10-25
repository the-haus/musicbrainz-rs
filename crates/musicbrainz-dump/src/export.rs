//! Where an export's files are, and what its metadata says.
//!
//! A path given to read from may be the archive itself
//! (`…/20261017-002244/mbdump.tar.bz2`), an export folder holding it (as
//! MetaBrainz publishes them, and as downloads are kept), or a folder of
//! tables taken out of it (`<dir>/mbdump/<table>` with the metadata files
//! beside `mbdump/`; Festimap keeps them in `<export>/tables`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::archive::Archive;
use crate::{DumpError, Result};

/// The archive of the CC0 core tables.
pub const CORE_ARCHIVE: &str = "mbdump.tar.bz2";

/// An export's files, as found from a path (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct ExportLayout {
    /// The folder of the export (`20261017-002244`), when there is one.
    pub dir: Option<PathBuf>,
    /// The archives found (`mbdump.tar.bz2`, `mbdump-derived.tar.bz2`, …).
    pub archives: Vec<PathBuf>,
    /// The folder holding `mbdump/` with tables taken out, when there is one.
    pub tables: Option<PathBuf>,
}

/// What an export's metadata files say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportInfo {
    /// When it was taken (`TIMESTAMP`: `2026-10-17 00:22:45.12891+00`).
    pub timestamp: Option<String>,
    /// Its schema sequence (`SCHEMA_SEQUENCE`).
    pub schema_sequence: Option<u32>,
    /// Its replication sequence (`REPLICATION_SEQUENCE`; empty for none).
    pub replication_sequence: Option<u64>,
    /// Its name: the folder's (`20261017-002244`), when it has one.
    pub name: Option<String>,
}

impl ExportInfo {
    /// The metadata from the files' contents.
    pub fn parse(timestamp: Option<&str>, schema: Option<&str>, replication: Option<&str>) -> ExportInfo {
        let clean = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        ExportInfo {
            timestamp: clean(timestamp),
            schema_sequence: clean(schema).and_then(|s| s.parse().ok()),
            replication_sequence: clean(replication).and_then(|s| s.parse().ok()),
            name: None,
        }
    }
}

fn looks_like_export_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 15 && b[8] == b'-' && b.iter().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit())
}

impl ExportLayout {
    /// Finds an export's files from a path (see the module docs).
    pub fn find(path: &Path) -> Result<ExportLayout> {
        let mut layout = ExportLayout::default();
        if path.is_file() {
            layout.archives.push(path.to_path_buf());
            layout.dir = path.parent().map(Path::to_path_buf);
        } else if path.join("mbdump").is_dir() {
            layout.tables = Some(path.to_path_buf());
            layout.dir = path
                .parent()
                .filter(|p| p.join(CORE_ARCHIVE).is_file())
                .map(Path::to_path_buf);
        } else if path.is_dir() {
            layout.dir = Some(path.to_path_buf());
        } else {
            return Err(DumpError::Io(format!("{}: no such file or folder", path.display())));
        }
        if let Some(dir) = layout.dir.clone() {
            if layout.archives.is_empty() {
                let mut found: Vec<PathBuf> = fs::read_dir(&dir)?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("mbdump") && n.ends_with(".tar.bz2"))
                    })
                    .collect();
                // The core first, then the others by name.
                found.sort_by_key(|p| (p.file_name().is_some_and(|n| n != CORE_ARCHIVE), p.clone()));
                layout.archives = found;
            }
            if layout.tables.is_none() {
                for t in [dir.join("tables"), dir.clone()] {
                    if t.join("mbdump").is_dir() {
                        layout.tables = Some(t);
                        break;
                    }
                }
            }
        }
        if layout.archives.is_empty() && layout.tables.is_none() {
            return Err(DumpError::Format(format!(
                "{}: no mbdump*.tar.bz2 archive and no mbdump/ folder of tables",
                path.display()
            )));
        }
        Ok(layout)
    }

    /// The core archive, when there is one.
    pub fn core_archive(&self) -> Option<&Path> {
        self.archives
            .iter()
            .find(|p| p.file_name().is_some_and(|n| n == CORE_ARCHIVE))
            .or(self.archives.first())
            .map(PathBuf::as_path)
    }

    /// The file of a table taken out, when it is there.
    pub fn table_file(&self, table: &str) -> Option<PathBuf> {
        let f = self.tables.as_ref()?.join("mbdump").join(table);
        f.is_file().then_some(f)
    }

    /// The export's name: its folder's, when that is an export's name.
    pub fn name(&self) -> Option<String> {
        let dir = self.dir.as_ref().or(self.tables.as_ref())?;
        dir.ancestors()
            .filter_map(|d| d.file_name().and_then(|n| n.to_str()))
            .find(|n| looks_like_export_name(n))
            .map(str::to_string)
    }

    /// The metadata: from the tables' folder when it has the files, else
    /// from the core archive's head.
    pub fn info(&self, threads: usize) -> Result<ExportInfo> {
        let mut info = None;
        if let Some(t) = &self.tables {
            let read = |n: &str| fs::read_to_string(t.join(n)).ok();
            let (ts, schema, rep) = (read("TIMESTAMP"), read("SCHEMA_SEQUENCE"), read("REPLICATION_SEQUENCE"));
            if schema.is_some() {
                info = Some(ExportInfo::parse(ts.as_deref(), schema.as_deref(), rep.as_deref()));
            }
        }
        if info.is_none() {
            if let Some(a) = self.core_archive() {
                info = Some(archive_info(&Archive::open(a)?, threads)?);
            }
        }
        let mut info = info.unwrap_or_default();
        info.name = self.name();
        Ok(info)
    }
}

/// The metadata files at an archive's head (it is read until the first
/// table: MetaBrainz writes them first).
pub fn archive_info(archive: &Archive, threads: usize) -> Result<ExportInfo> {
    let mut files: Vec<(String, String)> = Vec::new();
    archive.for_each(threads, |m, r| {
        if m.table.is_some() {
            return Ok(crate::archive::Flow::Stop);
        }
        if ["TIMESTAMP", "SCHEMA_SEQUENCE", "REPLICATION_SEQUENCE"].contains(&m.name.as_str()) {
            let mut s = String::new();
            r.read_to_string(&mut s)?;
            files.push((m.name.clone(), s));
        }
        Ok(crate::archive::Flow::Continue)
    })?;
    let get = |n: &str| files.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    Ok(ExportInfo::parse(
        get("TIMESTAMP"),
        get("SCHEMA_SEQUENCE"),
        get("REPLICATION_SEQUENCE"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::tests::archive_of;

    #[test]
    fn finds_archives_and_tables() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("20261017-002244");
        fs::create_dir_all(dir.join("tables/mbdump")).unwrap();
        fs::write(dir.join("tables/mbdump/artist"), "1\n").unwrap();
        fs::write(dir.join("tables/SCHEMA_SEQUENCE"), "31\n").unwrap();
        archive_of(&dir, &[("SCHEMA_SEQUENCE", b"31\n"), ("TIMESTAMP", b"t\n")]);
        for p in [dir.clone(), dir.join("tables"), dir.join(CORE_ARCHIVE)] {
            let l = ExportLayout::find(&p).unwrap();
            assert_eq!(l.name().as_deref(), Some("20261017-002244"), "{p:?}");
            assert!(l.core_archive().is_some(), "{p:?}");
        }
        let l = ExportLayout::find(&dir).unwrap();
        assert!(l.table_file("artist").is_some());
        assert_eq!(l.info(1).unwrap().schema_sequence, Some(31));
        let a = Archive::open(dir.join(CORE_ARCHIVE)).unwrap();
        assert_eq!(archive_info(&a, 1).unwrap().timestamp.as_deref(), Some("t"));
    }
}
