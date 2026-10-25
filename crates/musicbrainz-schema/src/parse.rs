//! Reading musicbrainz-server's SQL: `CREATE TABLE` statements for the
//! columns, `ALTER TABLE … PRIMARY KEY` and `… FOREIGN KEY` for the keys.
//! Only as much SQL as those files use is understood.

use crate::{Column, ForeignKey, Kind, Table};

/// The SQL could not be read.
#[derive(Debug, thiserror::Error)]
#[error("schema SQL: {0}")]
pub struct ParseError(pub String);

/// `t` named as the dumps name it: tables of `musicbrainz` bare, others
/// `<schema>.<table>`.
pub(crate) fn qualified(schema: &str, t: &str) -> String {
    let t = t.trim().trim_matches('"');
    match t.split_once('.') {
        Some(("musicbrainz", bare)) => bare.to_string(),
        Some(_) => t.to_string(),
        None if schema == "musicbrainz" => t.to_string(),
        None => format!("{schema}.{t}"),
    }
}

/// The SQL without `--` comments (quotes respected).
fn strip_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut quoted = false;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            quoted = !quoted;
        }
        if !quoted && c == '-' && chars.peek() == Some(&'-') {
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// `s` split on `sep` outside parentheses and quotes.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            c if c == sep && !quoted && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Words that end a column's type.
const AFTER_TYPE: &[&str] = &[
    "NOT",
    "NULL",
    "DEFAULT",
    "CHECK",
    "CONSTRAINT",
    "REFERENCES",
    "PRIMARY",
    "UNIQUE",
    "COLLATE",
    "GENERATED",
];

/// Words that start a table constraint rather than a column.
const CONSTRAINTS: &[&str] = &["CHECK", "CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN", "EXCLUDE", "LIKE"];

/// The enumerated types (`CREATE TYPE x AS ENUM`), read as text.
fn enum_types(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in split_top(sql, ';') {
        let words: Vec<&str> = stmt.split_whitespace().collect();
        if let [create, ty, name, ..] = words.as_slice() {
            if create.eq_ignore_ascii_case("CREATE") && ty.eq_ignore_ascii_case("TYPE") {
                out.push(name.trim_matches('"').to_string());
            }
        }
    }
    out
}

/// Every `CREATE TABLE` of a `CreateTables.sql`.
pub(crate) fn tables(schema: &str, sql: &str) -> Result<Vec<Table>, ParseError> {
    let sql = strip_comments(sql);
    let enums = enum_types(&sql);
    let mut out = Vec::new();
    for stmt in split_top(&sql, ';') {
        let stmt = stmt.trim();
        let Some(rest) = strip_prefix_ci(stmt, "CREATE TABLE") else {
            continue;
        };
        let open = rest
            .find('(')
            .ok_or_else(|| ParseError(format!("no columns: {}", first_line(stmt))))?;
        let name = rest[..open].trim().trim_start_matches("IF NOT EXISTS").trim();
        if name.contains(char::is_whitespace) {
            // `CREATE TABLE x PARTITION OF y …`: a partition, not a table
            // of its own (never dumped).
            continue;
        }
        let close = rest
            .rfind(')')
            .ok_or_else(|| ParseError(format!("unclosed: {}", first_line(stmt))))?;
        let body = &rest[open + 1..close];
        let mut columns = Vec::new();
        for item in split_top(body, ',') {
            let words: Vec<&str> = item.split_whitespace().collect();
            let Some((&col, tail)) = words.split_first() else {
                continue;
            };
            if CONSTRAINTS.iter().any(|k| col.eq_ignore_ascii_case(k)) {
                continue;
            }
            let ends = tail
                .iter()
                .position(|w| AFTER_TYPE.iter().any(|k| w.eq_ignore_ascii_case(k)))
                .unwrap_or(tail.len());
            let sql_type = tail[..ends].join(" ");
            let kind = Kind::of_sql(&sql_type, &enums)
                .ok_or_else(|| ParseError(format!("{name}.{col}: unknown type {sql_type:?}")))?;
            // What the column says of itself, up to a constraint (whose
            // `IS NOT NULL` is no column's `NOT NULL`).
            let mods = tail[ends..]
                .iter()
                .take_while(|w| {
                    let w = w.to_ascii_uppercase();
                    !w.starts_with("CHECK") && w != "CONSTRAINT"
                })
                .copied()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_uppercase();
            let serial = sql_type.to_ascii_uppercase().contains("SERIAL");
            columns.push(Column {
                name: col.trim_matches('"').to_string(),
                sql_type,
                kind,
                nullable: !serial && !mods.contains("NOT NULL") && !mods.contains("PRIMARY KEY"),
            });
        }
        out.push(Table {
            name: qualified(schema, name),
            schema: schema.to_string(),
            columns,
            primary_key: Vec::new(),
            foreign_keys: Vec::new(),
        });
    }
    Ok(out)
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
}

/// The table an `ALTER TABLE` statement alters, and the rest of it.
fn altered(stmt: &str) -> Option<(&str, &str)> {
    let rest = strip_prefix_ci(stmt.trim(), "ALTER TABLE")?.trim_start();
    let rest = strip_prefix_ci(rest, "ONLY").map(str::trim_start).unwrap_or(rest);
    let end = rest.find(char::is_whitespace)?;
    Some((&rest[..end], &rest[end..]))
}

/// The column names inside the first parentheses after `keyword`.
fn columns_after<'a>(s: &'a str, keyword: &str) -> Option<(Vec<String>, &'a str)> {
    let at = s.to_ascii_uppercase().find(keyword)?;
    let s = &s[at + keyword.len()..];
    let open = s.find('(')?;
    let close = open + s[open..].find(')')?;
    let cols = s[open + 1..close]
        .split(',')
        .map(|c| c.trim().trim_matches('"').to_string())
        .collect();
    Some((cols, &s[close + 1..]))
}

/// `(table, columns)` of each `PRIMARY KEY` constraint.
pub(crate) fn primary_keys(sql: &str) -> Vec<(String, Vec<String>)> {
    let sql = strip_comments(sql);
    split_top(&sql, ';')
        .into_iter()
        .filter_map(|stmt| {
            let (table, rest) = altered(stmt)?;
            let (cols, _) = columns_after(rest, "PRIMARY KEY")?;
            Some((table.to_string(), cols))
        })
        .collect()
}

/// `(table, key)` of each single-column `FOREIGN KEY` constraint (the
/// referenced table qualified as the dumps name it).
pub(crate) fn foreign_keys(sql: &str) -> Vec<(String, ForeignKey)> {
    let sql = strip_comments(sql);
    let mut schema = "musicbrainz".to_string();
    let mut out = Vec::new();
    for stmt in split_top(&sql, ';') {
        let trimmed = stmt.trim();
        if let Some(path) = strip_prefix_ci(trimmed, "SET search_path") {
            let path = path.trim_start_matches([' ', '=']).trim().trim_matches('\'');
            schema = path.split(',').next().unwrap_or("musicbrainz").trim().to_string();
            continue;
        }
        let Some((table, rest)) = altered(trimmed) else {
            continue;
        };
        let Some((cols, rest)) = columns_after(rest, "FOREIGN KEY") else {
            continue;
        };
        let Some(r) = strip_prefix_ci(rest.trim_start(), "REFERENCES") else {
            continue;
        };
        let r = r.trim_start();
        let Some(open) = r.find('(') else {
            continue;
        };
        let target = qualified(&schema, &r[..open]);
        let Some((refs, _)) = columns_after(&r[open..], "") else {
            continue;
        };
        if let ([col], [reference]) = (cols.as_slice(), refs.as_slice()) {
            out.push((
                qualified(&schema, table),
                ForeignKey {
                    column: col.clone(),
                    table: target,
                    references: reference.clone(),
                },
            ));
        }
    }
    // Tables are qualified by the caller with the file's schema; these are
    // qualified already.
    out.into_iter()
        .map(|(t, f)| (t.strip_prefix("musicbrainz.").unwrap_or(&t).to_string(), f))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_types_and_nulls() {
        let sql = "CREATE TYPE fluency AS ENUM ('basic', 'native');\n\
                   CREATE TABLE x ( -- replicate\n  id SERIAL, -- PK\n  name VARCHAR(255) NOT NULL DEFAULT '',\n  \
                   at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),\n  f fluency,\n  ids INTEGER[],\n  \
                   ended BOOLEAN NOT NULL DEFAULT FALSE\n    CONSTRAINT c CHECK (\n      (a IS NOT NULL OR\n  b IS NULL) AND ended = TRUE\n    ),\n  \
                   CHECK (name != ''),\n  text TEXT\n    CHECK (\n      (a IS NULL AND text IS NOT NULL)\n    )\n);";
        let t = &tables("musicbrainz", sql).unwrap()[0];
        let cols: Vec<_> = t
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.kind, c.nullable))
            .collect();
        assert_eq!(
            cols,
            vec![
                ("id", Kind::Integer, false),
                ("name", Kind::Text, false),
                ("at", Kind::Timestamp, true),
                ("f", Kind::Text, true),
                ("ids", Kind::IntegerArray, true),
                ("ended", Kind::Boolean, false),
                ("text", Kind::Text, true),
            ]
        );
    }

    #[test]
    fn keys() {
        let pks = primary_keys("ALTER TABLE a ADD CONSTRAINT a_pkey PRIMARY KEY (x, y);");
        assert_eq!(pks, vec![("a".to_string(), vec!["x".to_string(), "y".to_string()])]);
        let fks = foreign_keys(
            "SET search_path = 'cover_art_archive';\nALTER TABLE cover_art\n   ADD CONSTRAINT f\n   \
             FOREIGN KEY (release)\n   REFERENCES musicbrainz.release(id)\n   ON DELETE CASCADE;\n\
             ALTER TABLE cover_art_type ADD CONSTRAINT g FOREIGN KEY (type_id) REFERENCES art_type(id);",
        );
        assert_eq!(fks[0].0, "cover_art_archive.cover_art");
        assert_eq!(fks[0].1.table, "release");
        assert_eq!(fks[1].1.table, "cover_art_archive.art_type");
    }
}
