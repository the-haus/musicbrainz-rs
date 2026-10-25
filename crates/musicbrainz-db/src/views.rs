//! Views a database carries over the tables it loaded:
//!
//! - `mb_relationship`: every relationship table (`l_<a>_<b>`) in one,
//!   with its entity types and its link's type, dates and ended flag
//!   (`row_order`: the row's line in its table's dump);
//! - `mb_link_attribute`: each link's attributes, with their type's name,
//!   credit and text value.
//!
//! DuckDB pushes a filter on `entity0`/`entity1` down into each branch, so
//! asking for one entity's relationships reads only what it needs.

use musicbrainz_schema::Schema;

use crate::ddl::{ident, literal};

/// The `CREATE VIEW` statements for the tables loaded.
pub fn views(schema: &Schema, loaded: &[String]) -> Vec<String> {
    let has = |t: &str| loaded.iter().any(|l| l == t);
    let mut out = Vec::new();
    let links: Vec<_> = schema
        .tables()
        .iter()
        .filter(|t| has(&t.name))
        .filter_map(|t| t.link_entities().map(|(a, b)| (t, a, b)))
        .collect();
    if !links.is_empty() {
        let typed = has("link") && has("link_type");
        let branches: Vec<String> = links
            .iter()
            .map(|(t, a, b)| {
                let mut s = format!(
                    "SELECT {} AS link_table, {} AS entity0_type, {} AS entity1_type, r.id, r.link, \
                     r.entity0, r.entity1, r.link_order, r.entity0_credit, r.entity1_credit, r.last_updated, \
                     r.rowid AS row_order",
                    literal(&t.name),
                    literal(a),
                    literal(b)
                );
                if typed {
                    s.push_str(
                        ", l.link_type AS link_type_id, lt.gid AS link_type_gid, lt.name AS link_type, \
                         lt.link_phrase, lt.reverse_link_phrase, \
                         l.begin_date_year, l.begin_date_month, l.begin_date_day, \
                         l.end_date_year, l.end_date_month, l.end_date_day, l.ended, l.attribute_count",
                    );
                }
                s.push_str(&format!(" FROM {} r", ident(&t.name)));
                if typed {
                    s.push_str(" JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type");
                }
                s
            })
            .collect();
        out.push(format!(
            "CREATE OR REPLACE VIEW mb_relationship AS {}",
            branches.join(" UNION ALL ")
        ));
    }
    if has("link_attribute") && has("link_attribute_type") {
        let credit = if has("link_attribute_credit") {
            " LEFT JOIN link_attribute_credit c ON c.link = la.link AND c.attribute_type = la.attribute_type"
        } else {
            ""
        };
        let text = if has("link_attribute_text_value") {
            " LEFT JOIN link_attribute_text_value v ON v.link = la.link AND v.attribute_type = la.attribute_type"
        } else {
            ""
        };
        out.push(format!(
            "CREATE OR REPLACE VIEW mb_link_attribute AS SELECT la.link, la.rowid AS row_order, la.attribute_type AS attribute_type_id, \
             lat.gid AS attribute_type_gid, lat.name AS attribute_type, {} AS credited_as, {} AS text_value \
             FROM link_attribute la JOIN link_attribute_type lat ON lat.id = la.attribute_type{credit}{text}",
            if credit.is_empty() { "NULL" } else { "c.credited_as" },
            if text.is_empty() { "NULL" } else { "v.text_value" },
        ));
    }
    out
}
