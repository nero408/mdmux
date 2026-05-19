//! Manual visual smoke for the in-process renderer. Not a regression test —
//! the assertion-bearing test lives in `src/preview.rs`. This example just
//! dumps the line-by-line output so a human can spot-check spacing, table
//! borders, and inline emphasis. Run with:
//!
//! ```sh
//! cargo run --release --example render_smoke -- /path/to/file.md
//! ```

use std::path::PathBuf;

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: render_smoke <file.md>");

    let preview = mdmux::preview::load(&path).expect("failed to load file");
    let text = preview.render(Some(80));

    println!("--- {} lines rendered ---", text.lines.len());
    for (i, line) in text.lines.iter().enumerate() {
        let mut joined = String::new();
        for span in &line.spans {
            joined.push_str(&span.content);
        }
        println!("{i:3}: {joined}");
    }
}
