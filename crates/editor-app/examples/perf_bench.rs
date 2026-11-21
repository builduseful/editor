//! Quick zero-dependency performance benchmark for `editor-core` + `editor-render`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use editor_app::EditorApp;
use editor_core::{Document, SearchOptions};
use editor_platform::{BuiltinBitmapRasterizer, HeadlessHost, StdFileSystem};

fn main() {
    // 1. Create a 50 MB temporary text file (~1,000,000 lines).
    let tmp = std::env::temp_dir().join(format!("editor_perf_50mb_{}.rs", std::process::id()));
    {
        use std::io::{BufWriter, Write};
        let f = std::fs::File::create(&tmp).unwrap();
        let mut w = BufWriter::with_capacity(256 * 1024, f);
        for i in 0..1_000_000usize {
            writeln!(w, "pub fn line_{i:07}(x: u64) -> u64 {{ x + {i} }} // fast 🚀").unwrap();
        }
        w.flush().unwrap();
    }
    let file_size = std::fs::metadata(&tmp).unwrap().len();

    let fs = Arc::new(StdFileSystem);

    // 2. Measure lazy open + first line read (first-paint core latency).
    let t0 = Instant::now();
    let mut doc = Document::open(fs.as_ref(), &tmp).unwrap();
    let _first = doc.line_content(0).unwrap();
    let open_us = t0.elapsed().as_micros();

    // 3. Measure full SWAR indexing of all 1,000,000 lines (50+ MB).
    let t1 = Instant::now();
    let total_lines = doc.exact_line_count();
    let index_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // 4. Measure 10,000 sequential typing edits (with B-tree coalescing).
    let t2 = Instant::now();
    let mut pos = 0u64;
    for i in 0..10_000u64 {
        pos = doc.edit(pos, 0, "a", i, true).unwrap();
    }
    let edit_us = t2.elapsed().as_micros();

    // 5. Measure 10,000 random line lookups across 1,000,000 lines.
    let t3 = Instant::now();
    let mut acc = 0usize;
    for i in 0..10_000usize {
        let line = (i * 9973) % total_lines;
        if let Some((s, e, _)) = doc.line_byte_range(line) {
            acc += (e - s) as usize;
        }
    }
    let lookup_us = t3.elapsed().as_micros();

    // 6. Measure streaming literal search across the full 50+ MB file.
    let t4 = Instant::now();
    let hits = doc.find_all("line_0999999", SearchOptions::default());
    let search_ms = t4.elapsed().as_secs_f64() * 1000.0;

    // 7. Measure Retina 2240x1480 full frame + caret-blink partial frame + 10 MB single-line frame!
    let mut app = EditorApp::new(fs.clone(), 2240, 1480, 2.0);
    let mut host = HeadlessHost::new();
    let mut raster = BuiltinBitmapRasterizer;
    app.open_file(&tmp, &mut host).unwrap();
    app.render(&mut raster);
    let frames = 60;
    let t5 = Instant::now();
    for _ in 0..frames {
        app.mark_dirty();
        app.render(&mut raster);
    }
    let frame_ms = (t5.elapsed().as_secs_f64() * 1000.0) / frames as f64;

    // Caret-blink partial dirty-rect frame time!
    let blink_frames = 200;
    let t6 = Instant::now();
    for i in 0..blink_frames {
        let r = app.viewport.last_caret_rect;
        app.mark_dirty_rect(editor_platform::PixelRect::new(r.x - 4, r.y - 2, r.w + 8, r.h + 4));
        app.render(&mut raster);
        let _ = i;
    }
    let blink_us = t6.elapsed().as_micros() as f64 / blink_frames as f64;

    // 8. Pathological 10 MB single-line file open + render time!
    let tmp_single = std::env::temp_dir().join(format!("editor_perf_10mb_line_{}.json", std::process::id()));
    std::fs::write(&tmp_single, "a".repeat(10 * 1024 * 1024)).unwrap();
    let t7 = Instant::now();
    app.open_file(&tmp_single, &mut host).unwrap();
    app.render(&mut raster);
    let single_line_ms = t7.elapsed().as_secs_f64() * 1000.0;
    let _ = std::fs::remove_file(&tmp_single);

    println!("=== editor Rust v2 Performance Benchmark ===");
    println!("File size:                {:.2} MB ({} lines)", file_size as f64 / 1_048_576.0, total_lines);
    println!("Lazy open + 1st line:     {} us ({:.3} ms)", open_us, open_us as f64 / 1000.0);
    println!("Full SWAR index 60 MB:    {:.2} ms ({:.2} GB/s)", index_ms, (file_size as f64 / 1e9) / (index_ms / 1000.0));
    println!("10,000 typing edits:      {} us ({:.2} ns/edit)", edit_us, (edit_us as f64 * 1000.0) / 10_000.0);
    println!("10,000 random lookups:    {} us ({:.2} us/lookup, acc={})", lookup_us, lookup_us as f64 / 10_000.0, acc);
    println!("Literal search 60 MB:     {:.2} ms (hits={})", search_ms, hits.len());
    println!("Retina 2240x1480 frame:   {:.3} ms/frame ({:.0} FPS eq)", frame_ms, 1000.0 / frame_ms);
    println!("Caret-blink partial rect: {:.1} us/frame ({:.0} FPS eq)", blink_us, 1_000_000.0 / blink_us.max(0.1));
    println!("10 MB single-line open+draw: {:.3} ms", single_line_ms);

    let _ = std::fs::remove_file(&tmp);
    let _ = PathBuf::from("done");
}
