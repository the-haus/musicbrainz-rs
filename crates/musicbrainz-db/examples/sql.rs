//! Runs SQL on a database opened for writing, timing each statement:
//! `cargo run --release -p musicbrainz-db --example sql -- <db> "<sql>" …`.

use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: sql <db> <sql>…")?;
    let mut tmp = std::ffi::OsString::from(&path);
    tmp.push(".tmp");
    let config = duckdb::Config::default().with("temp_directory", tmp.to_string_lossy())?;
    let conn = duckdb::Connection::open_with_flags(&path, config)?;
    for sql in args {
        let t = Instant::now();
        conn.execute_batch(&sql)?;
        println!("{:>8.2} s  {sql}", t.elapsed().as_secs_f64());
    }
    Ok(())
}
