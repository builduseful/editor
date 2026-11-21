//! Quick zero-dependency performance benchmark for `editor-core` + `editor-render`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use editor_app::EditorApp;
use editor_core::{Document, SearchOptions};
use editor_platform::{BuiltinBitmapRasterizer, HeadlessHost, StdFileSystem};

#[cfg(target_os = "macos")]
fn phys_footprint_mb() -> f64 {
    #[repr(C)]
    struct TaskVmInfo {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
        min_address: u64,
        max_address: u64,
        _pad: [u64; 16],
    }
    extern "C" {
        static mach_task_self_: u32;
        fn task_info(
            target_task: u32,
            flavor: i32,
            task_info_out: *mut i32,
            task_info_outCnt: *mut u32,
        ) -> i32;
    }
    const TASK_VM_INFO: i32 = 22;
    let mut info: TaskVmInfo = unsafe { std::mem::zeroed() };
    let mut count = (std::mem::size_of::<TaskVmInfo>() / std::mem::size_of::<i32>()) as u32;
    let kr = unsafe {
        task_info(
            mach_task_self_,
            TASK_VM_INFO,
            (&mut info as *mut TaskVmInfo) as *mut i32,
            &mut count,
        )
    };
    if kr == 0 {
        info.phys_footprint as f64 / 1_048_576.0
    } else {
        0.0
    }
}

#[cfg(not(target_os = "macos"))]
fn phys_footprint_mb() -> f64 {
    0.0
}

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

    // 9. Comprehensive Syntax Highlighting Benchmarks (all plugins, incremental edit, 1M-line jump)!
    let syn_samples: &[(&str, &str)] = &[
        ("rust", "pub fn render<'a>(buf: &'a mut [u32], count: usize) -> Result<u64, String> { /* comment */ println!(\"ok {}\", 0xFF); Ok(42) }"),
        ("cpp", "#include <vector>\nstd::vector<int> compute(const std::string& s) { return { 1'000, 0xFF }; /* block */ }"),
        ("csharp", "[Serializable] public async Task<string> FetchAsync(int id) => $\"item-{id}\\n\"; // C#"),
        ("typescript", "export interface User<T> { readonly id: number; name: string; } const msg = `hi ${user.name}`;"),
        ("javascript", "const re = /[a-z]+/gi; export function greet(name) { return `Hello ${name}!`; }"),
        ("html", "<div class=\"card\" data-id=\"42\"><style>.card { color: #ff00aa; margin: 12px; }</style><script>const x = 1;</script></div>"),
        ("css", "@media (min-width: 768px) { .card:hover { color: var(--ink); padding: 12px 1.5rem !important; } }"),
        ("markdown", "# Heading **bold** *italic* `code` [link](https://example.com) and text"),
        ("python", "@dataclass\ndef compute(items: list[int], scale: float = 1.5) -> int: return sum(items) # py"),
        ("java", "@Override public final List<String> build(int n) { return List.of(\"alpha\\n\", \"beta\"); }"),
        ("kotlin", "data class Point(val x: Double, val y: Double) { fun norm() = x * x + y * y }"),
        ("go", "func Serve(ctx context.Context, addr string) error { ch := make(chan int, 16); return nil }"),
        ("sql", "SELECT u.id, COUNT(o.id) AS total FROM users u JOIN orders o ON o.uid = u.id WHERE o.sum > 99.5; -- ok"),
        ("php", "<?php function greet(string $name): string { return \"Hello $name\\n\"; } echo greet('world'); ?>"),
        ("shell", "export RUST_LOG=\"info\"; for f in *.rs; do echo \"checking $f\" && cargo test; done # sh"),
        ("powershell", "function Get-BuildInfo([string]$Root = $env:PWD) { <# comment #> Get-ChildItem -Path $Root -Recurse }"),
        ("c", "#define MAX_LEN 1024\nstatic inline uint32_t hash_u32(const uint8_t *p, size_t n) { return (uint32_t)(n ^ 0x9e3779b9u); }"),
    ];
    let iters_per_lang = 5_000usize;
    let mut total_lex_bytes = 0usize;
    let mut total_lex_spans = 0usize;
    let mut spans = Vec::with_capacity(64);
    let t_syn = Instant::now();
    for &(id, sample) in syn_samples {
        let lang = editor_core::Language::from_id(id).unwrap();
        let mut cache = editor_core::SyntaxCache::new(lang);
        let lines: Vec<&str> = sample.lines().collect();
        for it in 0..iters_per_lang {
            let line = lines[it % lines.len()];
            cache.lex_line(it % 64, line, &mut spans);
            total_lex_bytes += line.len();
            total_lex_spans += spans.len();
        }
    }
    let syn_sec = t_syn.elapsed().as_secs_f64();
    let syn_mb_s = (total_lex_bytes as f64 / 1_048_576.0) / syn_sec;
    let syn_ns_line = (syn_sec * 1e9) / (syn_samples.len() * iters_per_lang) as f64;

    // 1M-line file random jump to line 900,000 + syntax catch-up + cache memory check!
    let t_jump = Instant::now();
    doc.prepare_syntax_to(900_000);
    let jump_us = t_jump.elapsed().as_micros();

    // Incremental single-line edit convergence on 1M-line file!
    let t_inc = Instant::now();
    let (l_off, _, _) = doc.line_byte_range(900_000).unwrap();
    doc.edit(l_off, 0, "/* */ ", 999, false).unwrap();
    doc.prepare_syntax_to(900_001);
    let inc_us = t_inc.elapsed().as_micros();

    // Customized agent theme (all 19 tokens overridden) Retina frame time parity check!
    let custom_json = "{\"preset\":\"studio\",\"dark\":{\"keyword\":\"#ff7b72 bold\",\"comment\":\"#8b949e italic\",\"string\":\"#7ee787\",\"function\":\"#79c0ff\",\"type\":\"#d2a8ff\"}}";
    app.open_file(&tmp, &mut host).unwrap();
    app.set_theme_preference(editor_platform::ThemePreference::Dark);
    app.apply_syntax_theme_json(custom_json, &mut host).unwrap();
    app.render(&mut raster);
    let t_custom = Instant::now();
    for _ in 0..frames {
        app.mark_dirty();
        app.render(&mut raster);
    }
    let custom_frame_ms = (t_custom.elapsed().as_secs_f64() * 1000.0) / frames as f64;

    println!("=== editor Rust v2 Performance Benchmark ===");
    println!("File size:                {:.2} MB ({} lines)", file_size as f64 / 1_048_576.0, total_lines);
    println!("Lazy open + 1st line:     {} us ({:.3} ms)", open_us, open_us as f64 / 1000.0);
    println!("Full SWAR index 60 MB:    {:.2} ms ({:.2} GB/s)", index_ms, (file_size as f64 / 1e9) / (index_ms / 1000.0));
    println!("10,000 typing edits:      {} us ({:.2} ns/edit)", edit_us, (edit_us as f64 * 1000.0) / 10_000.0);
    println!("10,000 random lookups:    {} us ({:.2} us/lookup, acc={})", lookup_us, lookup_us as f64 / 10_000.0, acc);
    println!("Literal search 60 MB:     {:.2} ms (hits={})", search_ms, hits.len());
    println!("Retina 2240x1480 frame:   {:.3} ms/frame ({:.0} FPS eq)", frame_ms, 1000.0 / frame_ms);
    println!("Retina custom-theme frame:{:.3} ms/frame ({:.0} FPS eq)", custom_frame_ms, 1000.0 / custom_frame_ms);
    println!("Caret-blink partial rect: {:.1} us/frame ({:.0} FPS eq)", blink_us, 1_000_000.0 / blink_us.max(0.1));
    println!("10 MB single-line open+draw: {:.3} ms", single_line_ms);
    println!("Syntax lex (17 langs):    {:.1} MB/s ({:.0} ns/line, {} spans)", syn_mb_s, syn_ns_line, total_lex_spans);
    println!("Syntax 1M-line jump:      {} us ({:.3} ms)", jump_us, jump_us as f64 / 1000.0);
    println!("Syntax incremental edit:  {} us ({:.3} ms)", inc_us, inc_us as f64 / 1000.0);
    println!("macOS phys_footprint:     {:.2} MB (after 60 MB + 10 MB + Retina FB + 17 plugins)", phys_footprint_mb());

    let _ = std::fs::remove_file(&tmp);
    let _ = PathBuf::from("done");
}
