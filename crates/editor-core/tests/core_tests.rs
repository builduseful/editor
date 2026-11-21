use std::path::PathBuf;

use editor_core::document::find_matching_bracket;
use editor_core::swar::{count_line_breaks, find_byte, find_next_line_break, find_subslice, is_all_ascii};
use editor_core::{
    cluster_cell_width, grapheme_clusters, next_grapheme_boundary, next_word_boundary,
    prev_grapheme_boundary, prev_word_boundary, validate_utf8_offset, word_range_at, Document,
    DocumentError, Eol, Language, PieceTree, SearchOptions, Summary, SyntaxCache, TokenKind,
};
use editor_platform::MemoryFileSystem;

// Deterministic xorshift64 PRNG (zero external crates).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, max: usize) -> usize {
        if max == 0 {
            0
        } else {
            (self.next() as usize) % max
        }
    }
}

#[test]
fn swar_primitives_match_naive() {
    assert!(is_all_ascii(b"hello world 0123456789!@#"));
    assert!(!is_all_ascii("hello 🚀 world".as_bytes()));

    let data = b"0123456789abcdef\nghi\r\njkl\rmno";
    assert_eq!(find_byte(data, b'\n'), Some(16));
    assert_eq!(find_byte(data, b'\r'), Some(20));
    assert_eq!(find_byte(data, b'Z'), None);

    assert_eq!(find_next_line_break(data), Some((16, 1)));
    assert_eq!(find_next_line_break(&data[17..]), Some((3, 2))); // \r\n
    assert_eq!(find_next_line_break(&data[22..]), Some((3, 1))); // lone \r

    let counts = count_line_breaks(data);
    assert_eq!(counts.lf, 1);
    assert_eq!(counts.crlf, 1);
    assert_eq!(counts.cr, 1);
    assert_eq!(counts.total(), 3);

    assert_eq!(find_subslice(b"the quick brown fox", b"brown"), Some(10));
    assert_eq!(find_subslice(b"aaaaaab", b"aab"), Some(4));
    assert_eq!(find_subslice(b"short", b"longer"), None);
}

#[test]
fn summary_monoid_is_associative_with_split_crlf() {
    let sample = b"alpha\r\nbeta\r\ngamma\rdelta\nepsilon\r\n";
    let expected = Summary::from_bytes(sample);
    assert_eq!(expected.breaks, 5);

    // Split at EVERY possible byte position (including right between `\r` and `\n`!).
    for split in 0..=sample.len() {
        let left = Summary::from_bytes(&sample[..split]);
        let right = Summary::from_bytes(&sample[split..]);
        assert_eq!(
            left.combine(right),
            expected,
            "monoid failed at split={split}"
        );
    }
}

#[test]
fn grapheme_clusters_handle_ascii_crlf_and_emojis() {
    let s = "Hi👋🏽 🇿🇦👨‍👩‍👧‍👦 1️⃣ e\u{0301}\r\nfn";
    let clusters: Vec<&str> = grapheme_clusters(s).map(|(_, c)| c).collect();
    assert_eq!(
        clusters,
        vec![
            "H",
            "i",
            "👋🏽",
            " ",
            "🇿🇦",
            "👨‍👩‍👧‍👦",
            " ",
            "1️⃣",
            " ",
            "e\u{0301}",
            "\r\n",
            "f",
            "n",
        ]
    );

    // Verify exact forward/backward symmetry!
    let mut boundaries = vec![0usize];
    let mut pos = 0;
    while pos < s.len() {
        pos = next_grapheme_boundary(s, pos);
        boundaries.push(pos);
    }
    let mut rev = vec![s.len()];
    let mut rpos = s.len();
    while rpos > 0 {
        rpos = prev_grapheme_boundary(s, rpos);
        rev.push(rpos);
    }
    rev.reverse();
    assert_eq!(boundaries, rev);

    assert_eq!(cluster_cell_width("a"), 1);
    assert_eq!(cluster_cell_width("🚀"), 2);
    assert_eq!(cluster_cell_width("👨‍👩‍👧‍👦"), 2);
    assert_eq!(cluster_cell_width("🇿🇦"), 2);
}

#[test]
fn word_boundaries_and_utf8_validation() {
    let line = "  let foo_bar = 42 + baz;";
    assert_eq!(next_word_boundary(line, 0), 5); // past "  let"
    assert_eq!(next_word_boundary(line, 5), 13); // past " foo_bar"
    assert_eq!(prev_word_boundary(line, 13), 6); // start of "foo_bar"
    assert_eq!(word_range_at(line, 8), (6, 13)); // "foo_bar"

    assert_eq!(validate_utf8_offset("valid 🚀".as_bytes()), Ok(()));
    assert_eq!(validate_utf8_offset(&[b'a', b'b', 0xFF, b'c']), Err(2));
}

/// Naive reference helper to verify line offsets and contents against `PieceTree`.
fn naive_lines(buf: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < buf.len() {
        if buf[i] == b'\n' {
            out.push((start, i, i + 1));
            i += 1;
            start = i;
        } else if buf[i] == b'\r' {
            if i + 1 < buf.len() && buf[i + 1] == b'\n' {
                out.push((start, i, i + 2));
                i += 2;
                start = i;
            } else {
                out.push((start, i, i + 1));
                i += 1;
                start = i;
            }
        } else {
            i += 1;
        }
    }
    out.push((start, buf.len(), buf.len()));
    out
}

#[test]
fn piece_tree_randomized_oracle_matches_naive_across_1000_edits() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_BABE);
    let initial = "first line\r\nsecond line\nthird\rfourth 🚀";
    let mut tree = PieceTree::from_str(initial);
    let mut oracle = initial.as_bytes().to_vec();

    let snippets: &[&[u8]] = &[
        b"a",
        b"hello",
        b"\n",
        b"\r",
        b"\r\n",
        b"\n\r",
        b"line1\r\nline2\n",
        "🚀🎉".as_bytes(),
        b"   ",
    ];

    for step in 0..800 {
        let op = rng.range(3);
        if op < 2 || oracle.is_empty() {
            // Insert at a random valid boundary (keep UTF-8 valid by picking char boundary).
            let mut pos = rng.range(oracle.len() + 1);
            let s = std::str::from_utf8(&oracle).unwrap();
            while pos > 0 && !s.is_char_boundary(pos) {
                pos -= 1;
            }
            let ins = snippets[rng.range(snippets.len())];
            tree.insert(pos as u64, ins);
            oracle.splice(pos..pos, ins.iter().copied());
        } else {
            let s = std::str::from_utf8(&oracle).unwrap();
            let mut start = rng.range(oracle.len());
            while start > 0 && !s.is_char_boundary(start) {
                start -= 1;
            }
            let max_del = (oracle.len() - start).min(24);
            let mut end = start + rng.range(max_del + 1);
            while end < oracle.len() && !s.is_char_boundary(end) {
                end += 1;
            }
            let deleted = tree.delete(start as u64, (end - start) as u64);
            assert_eq!(deleted, oracle[start..end], "deleted bytes mismatch at step {step}");
            oracle.drain(start..end);
        }

        assert_eq!(tree.len_bytes() as usize, oracle.len(), "len mismatch at step {step}");
        if step % 10 == 0 {
            assert_eq!(
                tree.to_string_lossy().as_bytes(),
                oracle.as_slice(),
                "content mismatch at step {step}"
            );
            let expected_lines = naive_lines(&oracle);
            assert_eq!(
                tree.editable_line_count(),
                expected_lines.len(),
                "editable line count mismatch at step {step}"
            );
            for (li, &(es, ec, ef)) in expected_lines.iter().enumerate() {
                let got = tree.line_byte_range(li).unwrap();
                assert_eq!(
                    got,
                    (es as u64, ec as u64, ef as u64),
                    "line {li} range mismatch at step {step}"
                );
                let (bl, bc) = tree.byte_to_line_col(es as u64);
                assert_eq!((bl, bc), (li, 0), "byte_to_line_col start mismatch line {li}");
            }
        }
    }
}

#[test]
fn lazy_large_file_opens_without_full_scan_and_indexes_on_demand() {
    let fs = MemoryFileSystem::new();
    // Create a 1 MB file (> 256 KB inline threshold so paged + lazy path activates!).
    let mut big = String::with_capacity(1_050_000);
    for i in 0..20_000 {
        big.push_str(&format!("line {:05}: The quick brown fox jumps over the lazy dog.\n", i));
    }
    let path = PathBuf::from("/big.txt");
    fs.add_file(&path, big.as_bytes().to_vec());

    let mut doc = Document::open(&fs, &path).unwrap();
    assert!(!doc.is_fully_indexed(), "1 MB file should open lazily!");
    assert_eq!(doc.line_content(0).unwrap(), "line 00000: The quick brown fox jumps over the lazy dog.");
    assert!(!doc.is_fully_indexed(), "reading line 0 should not force full scan!");

    // Now jump far down to line 15,000.
    assert_eq!(
        doc.line_content(15_000).unwrap(),
        "line 15000: The quick brown fox jumps over the lazy dog."
    );
    // Exact line count forces full indexing.
    assert_eq!(doc.exact_line_count(), 20_000);
    assert!(doc.is_fully_indexed());
}

#[test]
fn document_bom_binary_utf16_undo_search_and_save() {
    let fs = MemoryFileSystem::new();
    // UTF-16 BOM rejected.
    fs.add_file("/utf16.txt", vec![0xFF, 0xFE, b'a', 0]);
    assert!(matches!(
        Document::open(&fs, &PathBuf::from("/utf16.txt")),
        Err(DocumentError::UnsupportedEncoding("utf-16"))
    ));

    // Binary file rejected.
    fs.add_file("/bin.dat", vec![b'h', b'i', 0, b'!']);
    assert!(matches!(
        Document::open(&fs, &PathBuf::from("/bin.dat")),
        Err(DocumentError::Binary { offset: 2 })
    ));

    // UTF-8 BOM accepted and preserved on save.
    let mut with_bom = vec![0xEF, 0xBB, 0xBF];
    with_bom.extend_from_slice(b"hello world\r\nsecond line\r\n");
    let bom_rev = fs.add_file("/bom.txt", with_bom);

    let mut doc = Document::open(&fs, &PathBuf::from("/bom.txt")).unwrap();
    assert!(doc.has_bom());
    assert_eq!(doc.eol(), Eol::CrLf);
    assert_eq!(doc.line_content(0).unwrap(), "hello world");

    // Coalescing typing run + undo/redo.
    let p1 = doc.edit(5, 0, "!", 100, true).unwrap();
    let _p2 = doc.edit(p1, 0, "?", 200, true).unwrap();
    assert_eq!(doc.line_content(0).unwrap(), "hello!? world");
    // Coalesced into ONE undo step!
    doc.undo();
    assert_eq!(doc.line_content(0).unwrap(), "hello world");
    doc.redo();
    assert_eq!(doc.line_content(0).unwrap(), "hello!? world");

    // Save preserves BOM and updates rev.
    let report = doc.save(&fs, Some(bom_rev), false).unwrap();
    assert_ne!(report.rev, bom_rev);
    let saved_raw = fs.read_bytes(&PathBuf::from("/bom.txt")).unwrap();
    assert!(saved_raw.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert_eq!(&saved_raw[3..], b"hello!? world\r\nsecond line\r\n");

    // Conflict detection with stale rev.
    doc.edit(0, 0, "X", 500, false).unwrap();
    assert!(matches!(
        doc.save(&fs, Some(bom_rev), false),
        Err(DocumentError::Conflict { .. })
    ));
}

#[test]
fn search_crosses_64kb_window_boundary_and_replace_all_undoes_atomically() {
    let mut text = "a".repeat(65_530);
    text.push_str("NEEDLE_ACROSS_BOUNDARY");
    text.push_str(&"b".repeat(100));
    text.push_str("needle_across_boundary");

    let mut doc = Document::from_str(&text, None);
    let cs = doc.find_all("NEEDLE_ACROSS_BOUNDARY", SearchOptions::default());
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].start, 65_530);

    let ci = doc.find_all(
        "needle_across_boundary",
        SearchOptions {
            case_sensitive: false,
            max_matches: 100,
        },
    );
    assert_eq!(ci.len(), 2);

    let count = doc.replace_all(
        "needle_across_boundary",
        "FOUND",
        SearchOptions {
            case_sensitive: false,
            max_matches: 100,
        },
        10,
    );
    assert_eq!(count, 2);
    assert!(doc.find_all("FOUND", SearchOptions::default()).len() == 2);

    // Single undo restores both replacements!
    doc.undo();
    assert_eq!(doc.to_string_lossy(), text);
}

#[test]
fn syntax_highlighter_handles_rust_nested_comments_and_brackets() {
    let rust = Language::from_id("rust").expect("rust plugin");
    let mut cache = SyntaxCache::new(rust);
    let mut spans = Vec::new();
    cache.lex_line(0, "pub fn main() { /* outer /* inner */", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));

    // Still inside outer block comment on line 1!
    cache.lex_line(1, "still comment */ let x: u64 = 42;", &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Comment);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Type));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Number));

    assert_eq!(find_matching_bracket("fn foo(a, (b)) {}", 6), Some(13));
    assert_eq!(find_matching_bracket("fn foo(a, (b)) {}", 13), Some(6));
}

#[test]
fn all_syntax_plugins_satisfy_contract() {
    use editor_core::{LanguageRegistry, LexState};
    use std::collections::BTreeSet;

    let reg = LanguageRegistry::global();
    assert!(reg.len() >= 20, "expected >= 20 compiled syntax plugins, got {}", reg.len());

    let mut seen_ids = BTreeSet::new();
    let stress_lines = [
        "",
        "   \t  ",
        "/* nested /* comment */ still */ // eol",
        "\"hello \\\"world\\\" \\u{1F680} 🚀👨‍👩‍👧‍👦\" 'a' `tmpl ${x + 1}`",
        "```rust",
        "fn greet<'a>(name: &'a str) -> Result<usize, String> { println!(\"{name}\"); Ok(42) }",
        "```",
        "<style>.card { color: #ff00aa; margin: 12px; }</style><script>const x = `hi ${1}`;</script>",
        "# Heading **bold** *em* `code` [link](https://example.com)",
        "SELECT id, name FROM users WHERE active = TRUE -- sql comment",
        "$env:PATH = \"C:\\bin\"; <# ps comment #> Get-ChildItem -Recurse",
        "<?php $x = 0xFF; echo \"hello\\n\"; ?>",
    ];
    let huge = "let x = 1; ".repeat(2_000); // > 16 KB guard test

    for lang in reg.all() {
        let meta = lang.meta();
        assert!(!meta.id.is_empty());
        assert!(!meta.name.is_empty());
        assert!(seen_ids.insert(meta.id), "duplicate plugin id: {}", meta.id);

        let mut cache = SyntaxCache::new(lang);
        let mut spans = Vec::new();
        let mut state = LexState::NORMAL;
        for (i, line) in stress_lines.iter().enumerate() {
            // Determinism + contract check directly via plugin.
            let mut a = Vec::new();
            let mut b = Vec::new();
            let next_a = reg.plugin(lang).lex_line(line, state, reg, &mut a);
            let next_b = reg.plugin(lang).lex_line(line, state, reg, &mut b);
            assert_eq!(next_a, next_b, "non-deterministic state in {}", meta.id);
            assert_eq!(a, b, "non-deterministic spans in {}", meta.id);

            let mut prev_end = 0usize;
            for sp in &a {
                let s = sp.start as usize;
                let e = sp.end as usize;
                assert!(s < e, "empty/inverted span in {}: {:?}", meta.id, sp);
                assert!(e <= line.len(), "OOB span in {}: {:?} > {}", meta.id, sp, line.len());
                assert!(s >= prev_end, "overlapping/unsorted span in {}: {:?} < {}", meta.id, sp, prev_end);
                assert!(line.is_char_boundary(s) && line.is_char_boundary(e), "non-UTF-8 span in {}", meta.id);
                prev_end = e;
            }
            state = next_a;
            cache.lex_line(i, line, &mut spans);
        }
        // Huge-line guard must return empty spans quickly without panicking.
        cache.lex_line(stress_lines.len(), &huge, &mut spans);
        assert!(spans.is_empty(), "huge line should skip lexing for {}", meta.id);
    }
}

#[test]
fn syntax_plugins_cover_all_requested_languages_and_embeddings() {
    use std::path::Path;

    // 1. Verify path detection for every requested language.
    let expected = [
        ("main.cpp", "cpp"),
        ("header.hpp", "cpp"),
        ("Program.cs", "csharp"),
        ("lib.rs", "rust"),
        ("README.md", "markdown"),
        ("index.html", "html"),
        ("styles.css", "css"),
        ("app.js", "javascript"),
        ("component.tsx", "typescript"),
        ("script.py", "python"),
        ("Main.java", "java"),
        ("deploy.sh", "shell"),
        ("build.ps1", "powershell"),
        ("server.go", "go"),
        ("schema.sql", "sql"),
        ("index.php", "php"),
        ("kernel.c", "c"),
        ("App.kt", "kotlin"),
    ];
    for (file, id) in expected {
        assert_eq!(Language::from_path(Some(Path::new(file))).id(), id, "wrong language for {file}");
    }

    // 2. HTML embedding `<style>` (CSS) and `<script>` (JS) + multi-line comments.
    let mut html = SyntaxCache::new(Language::from_id("html").unwrap());
    let mut spans = Vec::new();
    html.lex_line(0, "<!-- multi-line", &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Comment);
    html.lex_line(1, "comment --> <div class=\"card\">", &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Comment);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Tag));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Attribute));
    assert!(spans.iter().any(|s| s.kind == TokenKind::String));
    html.lex_line(2, "<style>", &mut spans);
    html.lex_line(3, "  .card { color: #ff00aa; margin: 12px; }", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Property), "CSS property inside <style>");
    assert!(spans.iter().any(|s| s.kind == TokenKind::Number), "CSS hex/unit inside <style>");
    html.lex_line(4, "</style>", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Tag));
    html.lex_line(5, "<script>", &mut spans);
    html.lex_line(6, "  const msg = `hello ${count + 1}`;", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword), "JS keyword inside <script>");
    assert!(spans.iter().any(|s| s.kind == TokenKind::String), "JS template inside <script>");
    assert!(spans.iter().any(|s| s.kind == TokenKind::Number), "JS expr inside template");
    html.lex_line(7, "</script>", &mut spans);

    // 3. Markdown headings, inline emphasis/code/links, and fenced ```rust delegation.
    let mut md = SyntaxCache::new(Language::from_id("markdown").unwrap());
    md.lex_line(0, "# Build Useful", &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Heading);
    md.lex_line(1, "A **bold** and *italic* `code` [link](https://example.com)", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Strong));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Emphasis));
    assert!(spans.iter().any(|s| s.kind == TokenKind::String));
    md.lex_line(2, "```rust", &mut spans);
    md.lex_line(3, "pub fn add(a: i32, b: i32) -> i32 { a + b }", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword), "Rust keyword inside ```rust fence");
    assert!(spans.iter().any(|s| s.kind == TokenKind::Type), "Rust type inside ```rust fence");
    assert!(spans.iter().any(|s| s.kind == TokenKind::Function), "Rust fn inside ```rust fence");
    md.lex_line(4, "```", &mut spans);

    // 4. PowerShell multi-line `<# ... #>` comment & variables/cmdlets.
    let mut ps = SyntaxCache::new(Language::from_id("powershell").unwrap());
    ps.lex_line(0, "<# block", &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Comment);
    ps.lex_line(1, "still comment #> function Get-Items($count) { return 42 }", &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Comment);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Function));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Number));

    // 5. C++ preprocessor + raw string + C# verbatim/attributes + Python triple string + SQL.
    let mut cpp = SyntaxCache::new(Language::from_id("cpp").unwrap());
    cpp.lex_line(0, "#include <vector>", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Macro));
    cpp.lex_line(1, "std::vector<int> v = { 1'000, 0xFF };", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Type));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Number));

    let mut cs = SyntaxCache::new(Language::from_id("csharp").unwrap());
    cs.lex_line(0, "[Serializable]", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Attribute));
    cs.lex_line(1, "public async Task<string> Run() => @\"raw\\path\";", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
    assert!(spans.iter().any(|s| s.kind == TokenKind::String));

    let mut py = SyntaxCache::new(Language::from_id("python").unwrap());
    py.lex_line(0, "@dataclass", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Attribute));
    py.lex_line(1, "def f(x: int) -> str:", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Function));

    let mut sql = SyntaxCache::new(Language::from_id("sql").unwrap());
    sql.lex_line(0, "SELECT COUNT(*) FROM orders WHERE total > 99.5 -- ok", &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Function));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Number));
    assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));

    // 6. Incremental SyntaxCache catch-up & edit invalidation across lines!
    let fs = MemoryFileSystem::new();
    let p = PathBuf::from("/multi.rs");
    fs.add_file(&p, b"/*\nline1\nline2\n*/\nlet x = 1;\n".to_vec());
    let mut doc = Document::open(&fs, &p).unwrap();
    doc.prepare_syntax_to(4);
    let line4 = doc.line_content(4).unwrap();
    doc.lex_line(4, &line4, &mut spans);
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
    // Remove `*/` on line 3 so line 4 becomes part of the block comment!
    let (l3_s, l3_e, _) = doc.line_byte_range(3).unwrap();
    doc.edit(l3_s, l3_e - l3_s, "", 1, false).unwrap();
    doc.prepare_syntax_to(4);
    doc.lex_line(4, &line4, &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Comment, "edit invalidation must re-lex downstream lines");

    // 7. Blank lines inside multi-line constructs MUST preserve cross-line LexState!
    let mut blank_cache = SyntaxCache::new(Language::from_id("rust").unwrap());
    blank_cache.lex_line(0, "/* start comment", &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Comment);
    blank_cache.lex_line(1, "", &mut spans);
    assert!(spans.is_empty());
    blank_cache.lex_line(2, "still inside comment */ let ok = 1;", &mut spans);
    assert_eq!(spans[0].kind, TokenKind::Comment, "blank line must carry block comment state");
    assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));

    // 8. Windowed SyntaxCache keeps memory tiny (~256 lines) on random jumps into 500k-line files!
    let mut huge_cache = SyntaxCache::new(Language::from_id("rust").unwrap());
    huge_cache.catch_up_to(500_000, |_| Some("let x: usize = 42;".to_string()));
    assert!(
        huge_cache.cached_len() <= 300,
        "windowed SyntaxCache should stay compact on huge jumps, got {}",
        huge_cache.cached_len()
    );
}

#[test]
fn lockfile_external_identity_windowed_huge_line_and_multiline_brackets() {
    use editor_core::IdentityCheck;
    use editor_platform::{FileSystem, FsError};

    let fs = MemoryFileSystem::new();
    let p = PathBuf::from("/doc.rs");
    fs.add_file(&p, b"fn outer() {\n    let v = [1, 2, 3];\n}\n".to_vec());

    // 1. Advisory lockfile prevents concurrent lock until dropped.
    let guard = fs.lock_file(&p).unwrap();
    assert!(matches!(fs.lock_file(&p), Err(FsError::Locked(_))));
    drop(guard);
    assert!(fs.lock_file(&p).is_ok());

    // 2. External mutation detection: auto-reload when clean, stale when dirty.
    let mut doc = Document::open(&fs, &p).unwrap();
    assert_eq!(doc.check_identity(&fs), IdentityCheck::Unchanged);
    fs.add_file(&p, b"fn updated() {\n    let x = {\n        1\n    };\n}\n".to_vec());
    assert_eq!(doc.check_identity(&fs), IdentityCheck::ReloadedClean);
    assert_eq!(doc.line_content(0).unwrap(), "fn updated() {");

    // Multi-line bracket matching across lines!
    let open_pos = doc.line_content(0).unwrap().find('{').unwrap() as u64;
    let pair = doc.find_matching_bracket(open_pos).unwrap();
    assert_eq!(pair.0, open_pos);
    let close_ch = doc.read_range(pair.1, 1);
    assert_eq!(close_ch, b"}");

    // Dirty doc detects external mutation without discarding unsaved edits.
    doc.edit(0, 0, "// local\n", 10, false).unwrap();
    fs.add_file(&p, b"external overwrite\n".to_vec());
    assert_eq!(doc.check_identity(&fs), IdentityCheck::StaleDirty);
    assert_eq!(doc.line_content(0).unwrap(), "// local");

    // 3. Pathological 2 MB single-line file windowed read (never allocates full line!).
    let mut huge_line = "a".repeat(1_000_000);
    huge_line.push_str("🚀MIDDLE🚀");
    huge_line.push_str(&"b".repeat(1_000_000));
    let huge_path = PathBuf::from("/single_line.json");
    fs.add_file(&huge_path, huge_line.as_bytes().to_vec());

    let mut huge_doc = Document::open(&fs, &huge_path).unwrap();
    let (actual_off, full_len, win) = huge_doc.read_line_window(0, 1_000_001, 64).unwrap();
    assert_eq!(full_len, huge_line.len() as u64);
    assert_eq!(actual_off, 1_000_000, "must snap back to emoji UTF-8 leading byte!");
    assert!(win.starts_with("🚀MIDDLE🚀"));
    assert!(win.len() <= 64);
}

#[test]
fn piece_tree_heavy_delete_compaction_5000_steps() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    let mut tree = PieceTree::from_str(&"line of text\n".repeat(400));
    let mut oracle = "line of text\n".repeat(400).into_bytes();

    for step in 0..2500 {
        // Bias towards deletes periodically to exercise sparse leaf merging!
        let do_delete = !oracle.is_empty() && (step % 5 != 0 || oracle.len() > 8000);
        if do_delete {
            let start = rng.range(oracle.len());
            let del_len = rng.range((oracle.len() - start).min(64) + 1);
            let got = tree.delete(start as u64, del_len as u64);
            assert_eq!(got, oracle[start..start + del_len]);
            oracle.drain(start..start + del_len);
        } else {
            let pos = rng.range(oracle.len() + 1);
            let ins = if step % 3 == 0 { b"xyz\r\n".as_slice() } else { b"hello\n".as_slice() };
            tree.insert(pos as u64, ins);
            oracle.splice(pos..pos, ins.iter().copied());
        }
        assert_eq!(tree.len_bytes() as usize, oracle.len());
    }
    assert_eq!(tree.to_string_lossy().as_bytes(), oracle.as_slice());
}
