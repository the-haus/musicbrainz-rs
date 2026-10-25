//! Prints each table and its columns: `cargo run --example columns [table…]`.

fn main() {
    let schema = musicbrainz_schema::Schema::bundled();
    let wanted: Vec<String> = std::env::args().skip(1).collect();
    for t in schema.tables() {
        if wanted.is_empty() || wanted.contains(&t.name) {
            let cols: Vec<_> = t.columns.iter().map(|c| c.name.as_str()).collect();
            println!("{}\t{}\t{}", t.name, cols.len(), cols.join(" "));
        }
    }
}
