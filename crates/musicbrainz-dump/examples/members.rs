//! Lists an archive's members and how long the pass took:
//! `cargo run --release --example members -- mbdump.tar.bz2`.

use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: members <archive>")?;
    let started = Instant::now();
    let archive = musicbrainz_dump::Archive::open(&path)?;
    let mut total = 0u64;
    for m in archive.members(0)? {
        total += m.size;
        println!("{}\t{}", m.name, m.size);
    }
    let s = started.elapsed().as_secs_f64();
    eprintln!(
        "{:.1} GB in {s:.1} s ({:.0} MB/s decompressed)",
        total as f64 / 1e9,
        total as f64 / 1e6 / s
    );
    Ok(())
}
