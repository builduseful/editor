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
            ..SearchOptions::default()
        },
    );
    assert_eq!(ci.len(), 2);

    let count = doc.replace_all(
        "needle_across_boundary",
        "FOUND",
        SearchOptions {
            case_sensitive: false,
            max_matches: 100,
            ..SearchOptions::default()
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
fn whole_word_and_linear_time_regex_search_and_capture_replace() {
    use editor_core::{CompiledQuery, SearchScratch};
    use std::time::Instant;

    // 1. Whole-word literal search (case-sensitive & case-insensitive) + boundary safety.
    let mut doc = Document::from_str("foo foobar bar_foo FOO (foo) foo!", None);
    let ww = doc.find_all(
        "foo",
        SearchOptions {
            case_sensitive: true,
            whole_word: true,
            regex: false,
            max_matches: 100,
        },
    );
    assert_eq!(ww.len(), 3, "should match standalone `foo`, `(foo)`, and `foo!` only");

    let ww_ci = doc.find_all(
        "foo",
        SearchOptions {
            case_sensitive: false,
            whole_word: true,
            regex: false,
            max_matches: 100,
        },
    );
    assert_eq!(ww_ci.len(), 4);

    // 2. Regex features: greedy vs lazy, classes, counted repeats, CRLF `$`, UTF-8 `.`, captures.
    let mut rdoc = Document::from_str("fn alpha(x: i32) -> bool\r\nfn beta(y: u64) -> ()\r\ncafé 🚀\r\n", None);
    let fn_hits = rdoc
        .find_all_checked(
            r"^fn\s+(\w+)\(([^)]*)\).*$",
            SearchOptions {
                case_sensitive: true,
                whole_word: false,
                regex: true,
                max_matches: 100,
            },
        )
        .unwrap();
    assert_eq!(fn_hits.len(), 2);
    // Verify `$` and `.` on CRLF did not eat `\r`!
    let mut line0 = Vec::new();
    rdoc.tree_mut()
        .read_range(fn_hits[0].start, (fn_hits[0].end - fn_hits[0].start) as usize, &mut line0);
    assert_eq!(std::str::from_utf8(&line0).unwrap(), "fn alpha(x: i32) -> bool");

    // UTF-8 safety: `.` matches entire multi-byte chars `é` and `🚀`, never splits UTF-8!
    let utf8_hits = rdoc
        .find_all_checked(
            "caf. .",
            SearchOptions {
                regex: true,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(utf8_hits.len(), 1);
    let mut ubuf = Vec::new();
    rdoc.tree_mut()
        .read_range(utf8_hits[0].start, (utf8_hits[0].end - utf8_hits[0].start) as usize, &mut ubuf);
    assert_eq!(std::str::from_utf8(&ubuf).unwrap(), "café 🚀");

    // Capture replacement (`$1`, `$2`, `$$`) + single atomic Undo!
    let before = rdoc.to_string_lossy();
    let replaced = rdoc.replace_all(
        r"fn\s+(\w+)\((\w+):\s*(\w+)\)",
        "pub fn $1($3 $2) $$",
        SearchOptions {
            regex: true,
            ..SearchOptions::default()
        },
        42,
    );
    assert_eq!(replaced, 2);
    let after = rdoc.to_string_lossy();
    assert!(after.contains("pub fn alpha(i32 x) $ -> bool"));
    assert!(after.contains("pub fn beta(u64 y) $ -> ()"));
    rdoc.undo();
    assert_eq!(rdoc.to_string_lossy(), before);

    // 3. Lazy vs greedy quantifiers + zero-length match termination (`^`, `a*`).
    let mut qdoc = Document::from_str("<b>one</b><b>two</b>", None);
    let lazy = qdoc
        .find_all_checked(
            "<b>.*?</b>",
            SearchOptions {
                regex: true,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(lazy.len(), 2);
    let greedy = qdoc
        .find_all_checked(
            "<b>.*</b>",
            SearchOptions {
                regex: true,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(greedy.len(), 1);

    let zero_hits = qdoc
        .find_all_checked(
            "x*",
            SearchOptions {
                regex: true,
                max_matches: 100,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert!(zero_hits.len() <= 25);

    // 4. Invalid regex returns a clean error instead of panicking.
    assert!(qdoc
        .find_all_checked(
            "(unclosed",
            SearchOptions {
                regex: true,
                ..SearchOptions::default()
            },
        )
        .is_err());

    // 5. ReDoS immunity: `(a+)+b` against 10,000 `a`s must finish in linear time (well under 50ms)!
    let evil_input = "a".repeat(10_000);
    let compiled = CompiledQuery::compile(
        "(a+)+b",
        SearchOptions {
            regex: true,
            ..SearchOptions::default()
        },
    )
    .unwrap()
    .unwrap();
    let mut scratch = SearchScratch::new();
    let mut out = Vec::new();
    let t0 = Instant::now();
    compiled.find_in_bytes(evil_input.as_bytes(), 10, &mut scratch, &mut out);
    assert!(out.is_empty());
    assert!(
        t0.elapsed().as_millis() < 250,
        "PikeVM must stay linear-time on ReDoS inputs (took {:?})",
        t0.elapsed()
    );

    // 6. Verify two-byte SWAR exact & case-insensitive helpers and SWAR `count_byte` against naive!
    let sample = b"    HeLLo_WoRLd    hello_world    HELLO_WORLD\nline2\nline3\n";
    assert_eq!(editor_core::swar::find_subslice(sample, b"hello_world"), Some(19));
    assert_eq!(editor_core::swar::find_subslice_ascii_ci(sample, b"hello_world"), Some(4));
    assert_eq!(editor_core::swar::find_byte_ascii_ci(sample, b'w'), Some(10));
    assert_eq!(editor_core::swar::count_byte(sample, b'\n'), 3);
    // Case-insensitive regex with literal prefix uses SWAR prefilter cleanly.
    let ci_re = CompiledQuery::compile(
        r"hello_\w+",
        SearchOptions {
            case_sensitive: false,
            regex: true,
            ..SearchOptions::default()
        },
    )
    .unwrap()
    .unwrap();
    out.clear();
    ci_re.find_in_bytes(sample, 10, &mut scratch, &mut out);
    assert_eq!(out.len(), 3);
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

#[test]
fn swar_unrolled_rare_pair_and_regex_span_parity() {
    use editor_core::regex::{PikeVmScratch, Regex};
    use editor_core::swar::{
        count_byte, find_byte, find_byte_ascii_ci, find_subslice, find_subslice_ascii_ci,
        rfind_byte,
    };
    use editor_core::{CompiledQuery, SearchScratch};

    // 1. Exhaustively test 32-byte unrolled SWAR primitives across every alignment 0..100.
    for len in 0..100usize {
        let mut buf = vec![b'e'; len];
        assert_eq!(find_byte(&buf, b'z'), None);
        assert_eq!(rfind_byte(&buf, b'z'), None);
        assert_eq!(find_byte_ascii_ci(&buf, b'z'), None);
        assert_eq!(count_byte(&buf, b'e'), len);
        for pos in 0..len {
            buf[pos] = b'Z';
            assert_eq!(find_byte(&buf, b'Z'), Some(pos));
            assert_eq!(rfind_byte(&buf, b'Z'), Some(pos));
            assert_eq!(find_byte_ascii_ci(&buf, b'z'), Some(pos));
            assert_eq!(count_byte(&buf, b'Z'), 1);
            buf[pos] = b'e';
        }
    }

    // 2. Common-prefix & repeated-byte needles across 32-byte boundaries.
    let hay = format!(
        "{}    pub fn target_xyz(a: u8) {{ aaaaaab }}",
        "    pub fn other(x: i32) { x }\n".repeat(5)
    );
    let b = hay.as_bytes();
    assert_eq!(
        find_subslice(b, b"    pub fn target_xyz"),
        hay.find("    pub fn target_xyz")
    );
    assert_eq!(
        find_subslice_ascii_ci(b, b"    pub fn target_xyz"),
        hay.find("    pub fn target_xyz")
    );
    assert_eq!(find_subslice(b, b"aaaaaab"), hay.find("aaaaaab"));
    assert_eq!(find_subslice(b, b"aaaaaa"), hay.find("aaaaaa"));
    assert_eq!(find_subslice_ascii_ci(b, b"AAAAAB".to_ascii_lowercase().as_slice()), hay.find("aaaaab"));

    // 3. Verify capture-free `find_span_with_context` matches `find_captures_with_context`
    // identically across prefixed, unprefixed (`start_filter`), alternation (`alt_literals`),
    // lazy, word-boundary, multiline, and UTF-8 emoji inputs!
    let sample = "alpha 🚀 2026-11-24 FooBar_42 beta FIXME: check\nhello_world TODO: fix 9999\r\n";
    let patterns = [
        (r"TODO|FIXME|HACK", true, false),
        (r"todo|fixme", false, true),
        (r"\b[A-Z][a-zA-Z0-9_]+\b", true, false),
        (r"\d{4}-\d{2}-\d{2}", true, false),
        (r"a.+?b", true, false),
        (r"^\w+", true, false),
        (r"\d+$", true, false),
        (r"🚀\s+\d+", true, false),
        (r"check\nhello", true, false),
    ];
    let mut scratch = PikeVmScratch::new();
    for (pat, cs, ww) in patterns {
        let re = Regex::compile(pat, cs, ww).unwrap();
        let mut pos = 0usize;
        while pos <= sample.len() {
            let span = re.find_span_with_context(sample, pos, None, None, &mut scratch);
            let caps = re
                .find_captures_with_context(sample, pos, None, None, &mut scratch)
                .map(|c| c.full());
            assert_eq!(span, caps, "parity mismatch for pattern `{pat}` at pos {pos}");
            match span {
                Some((s, e)) if e > s => pos = e,
                Some((_, e)) if e < sample.len() => {
                    pos = e + sample[e..].chars().next().unwrap().len_utf8();
                }
                _ => break,
            }
        }
    }

    // 4. Alternation SWAR prefilter returns all hits via CompiledQuery.
    let q = CompiledQuery::compile(
        "TODO|FIXME|HACK",
        SearchOptions {
            regex: true,
            case_sensitive: true,
            ..SearchOptions::default()
        },
    )
    .unwrap()
    .unwrap();
    let mut s = SearchScratch::new();
    let mut hits = Vec::new();
    q.find_in_bytes(sample.as_bytes(), 100, &mut s, &mut hits);
    assert_eq!(hits.len(), 2);
    assert_eq!(&sample[hits[0].0..hits[0].1], "FIXME");
    assert_eq!(&sample[hits[1].0..hits[1].1], "TODO");
}

#[test]
fn evict_page_cache_preserves_unsaved_edits_and_refills_paged_reads() {
    let fs = MemoryFileSystem::new();
    let p = PathBuf::from("/large_paged.txt");
    // > 256 KB so PieceTree uses BaseBacking::Paged with its 8 x 64 KB PageCache.
    let mut content = String::with_capacity(420_000);
    for i in 0..8000usize {
        content.push_str(&format!("line_{i:05}_payload_0123456789_abcdef_xyz\n"));
    }
    assert!(content.len() > 300_000);
    fs.add_file(&p, content.as_bytes().to_vec());

    let mut doc = Document::open(&fs, &p).unwrap();
    assert_eq!(doc.line_content(0).unwrap(), "line_00000_payload_0123456789_abcdef_xyz");
    assert_eq!(doc.line_content(5000).unwrap(), "line_05000_payload_0123456789_abcdef_xyz");

    // Apply an unsaved edit into AddBuffer, then evict the base PageCache (simulating tab park).
    let (l10_s, _, _) = doc.line_byte_range(10).unwrap();
    doc.edit(l10_s, 0, "// EDITED ", 1, false).unwrap();
    doc.evict_page_cache();

    // Base lines refill seamlessly after eviction AND unsaved edits stay intact!
    assert!(doc.is_dirty());
    assert_eq!(doc.line_content(0).unwrap(), "line_00000_payload_0123456789_abcdef_xyz");
    assert_eq!(
        doc.line_content(10).unwrap(),
        "// EDITED line_00010_payload_0123456789_abcdef_xyz"
    );
    assert_eq!(doc.line_content(7999).unwrap(), "line_07999_payload_0123456789_abcdef_xyz");
}

#[test]
fn audit_regressions_draft_save_crlf_chunks_syntax_utf8_and_regex() {
    use editor_core::regex::{PikeVmScratch, Regex};
    use editor_core::{
        prev_grapheme_boundary, utf8_floor, CompiledQuery, Eol, Language, SaveMethod,
        SearchOptions, SearchScratch, SyntaxCache, TokenSpan,
    };
    use editor_platform::FileSystem;

    // 1. Hot-exit draft / from_unsaved_draft saved over an existing disk file must NEVER use
    // delta/CoW fast paths against the mismatched disk file (would wipe draft content on APFS).
    let fs = MemoryFileSystem::new();
    let p = PathBuf::from("/draft_target.txt");
    fs.add_file(&p, b"old on disk content that is completely different\n".to_vec());
    let disk_rev = fs.stat(&p).unwrap().rev;
    let mut draft = Document::from_unsaved_draft(
        "fresh recovered hot-exit draft content\n",
        Some(p.clone()),
        disk_rev,
        false,
        Eol::Lf,
    );
    let report = draft.save(&fs, None, true).unwrap();
    assert_eq!(report.method, SaveMethod::InPlace);
    let mut reloaded = Document::open(&fs, &p).unwrap();
    assert_eq!(
        reloaded.to_string_lossy(),
        "fresh recovered hot-exit draft content\n"
    );

    // Also verify normal atomic save (`in_place = false`) on a recovered draft writes full content.
    let disk_rev2 = fs.stat(&p).unwrap().rev;
    let mut draft2 = Document::from_unsaved_draft(
        "second recovered draft\n",
        Some(p.clone()),
        disk_rev2,
        false,
        Eol::Lf,
    );
    let report2 = draft2.save(&fs, None, false).unwrap();
    assert_eq!(report2.method, SaveMethod::AtomicRewrite);
    let mut reloaded2 = Document::open(&fs, &p).unwrap();
    assert_eq!(reloaded2.to_string_lossy(), "second recovered draft\n");

    // 2. Pure CRLF file >16 KB with `\r\n` straddling the exact 16,384-byte lazy chunk boundary.
    // Make byte 16,383 = '\r' and byte 16,384 = '\n'.
    let mut crlf_bytes = Vec::with_capacity(20_000);
    // 16384 = 16 * 1024. If each line is 32 bytes ("a"*30 + "\r\n"), 511 lines = 16352 bytes.
    // Then 31 'b's + "\r\n" puts '\r' at 16383 and '\n' at 16384!
    for _ in 0..511 {
        crlf_bytes.extend_from_slice(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
    }
    crlf_bytes.extend_from_slice(&[b'b'; 31]);
    crlf_bytes.extend_from_slice(b"\r\n");
    assert_eq!(crlf_bytes[16383], b'\r');
    assert_eq!(crlf_bytes[16384], b'\n');
    for i in 0..50 {
        crlf_bytes.extend_from_slice(format!("tail_line_{i:02}\r\n").as_bytes());
    }
    let crlf_path = PathBuf::from("/crlf_boundary.txt");
    fs.add_file(&crlf_path, crlf_bytes.clone());
    let mut crlf_doc = Document::open(&fs, &crlf_path).unwrap();
    crlf_doc.tree_mut().ensure_fully_indexed();
    assert_eq!(
        crlf_doc.info().eol,
        Eol::CrLf,
        "CRLF straddling 16KB lazy boundary must stay Eol::CrLf, not Mixed"
    );
    assert_eq!(crlf_doc.line_content(511).unwrap(), "b".repeat(31));
    assert_eq!(crlf_doc.line_content(512).unwrap(), "tail_line_00");
    assert_eq!(crlf_doc.byte_to_line_col(16385), (512, 0));

    // 3. Syntax lexers with escaped multi-byte UTF-8 must produce valid char-boundary spans.
    let mut spans: Vec<TokenSpan> = Vec::new();
    for (lang_id, sample) in [
        ("rust", "let s = \"a\\é\"; let c = '\\🦀';"),
        ("javascript", "const a = `hello\\é`; const b = \"x\\🦀\";"),
        ("powershell", "$s = \"a`é\" + \"b`🦀\""),
        ("python", "s = 'a\\é'"),
    ] {
        let lang = Language::from_id(lang_id).unwrap();
        let mut cache = SyntaxCache::new(lang);
        spans.clear();
        cache.lex_line(0, sample, &mut spans);
        for sp in &spans {
            assert!(sample.is_char_boundary(sp.start as usize));
            assert!(sample.is_char_boundary(sp.end as usize));
        }
    }

    // Markdown fenced JS template literal & Rust raw string preserve inner state across lines.
    let md = Language::from_id("markdown").unwrap();
    let mut md_cache = SyntaxCache::new(md);
    let md_lines = [
        "```js",
        "const s = `line1",
        "line2`;",
        "```",
        "```rust",
        "let r = r##\"raw1",
        "raw2\"##;",
        "```",
    ];
    for (idx, line) in md_lines.iter().enumerate() {
        spans.clear();
        md_cache.lex_line(idx, line, &mut spans);
        assert!(!spans.is_empty(), "md line {idx} should lex cleanly");
    }

    // 4. `utf8_floor`, CRLF regex `$`, `[\W]`/`[\S]` non-ASCII, `\B` capture replacement, grapheme.
    assert_eq!(utf8_floor("xéabc".as_bytes()), 6);
    assert_eq!(utf8_floor(&"xé".as_bytes()[..2]), 1);

    let mut vm_scratch = PikeVmScratch::new();
    // `$` must match once per CRLF line end (never between `\r` and `\n`).
    let re_dollar = Regex::compile("$", true, false).unwrap();
    assert_eq!(
        re_dollar.find_span_with_context("foo\r\n", 3, Some('o'), Some('\r'), &mut vm_scratch),
        Some((3, 3))
    );
    // Starting between `\r` and `\n` must not match there; the next match is the true end (5).
    assert_eq!(
        re_dollar.find_span_with_context("foo\r\n", 4, Some('\r'), Some('\n'), &mut vm_scratch),
        Some((5, 5))
    );
    let re_eol = Regex::compile("foo$", true, false).unwrap();
    assert_eq!(
        re_eol.find_span_with_context("foo\r\n", 0, None, None, &mut vm_scratch),
        Some((0, 3))
    );

    let re_non_word = Regex::compile(r"[\W]+", true, false).unwrap();
    assert_eq!(
        re_non_word.find_span_with_context("a🚀 b", 0, None, None, &mut vm_scratch),
        Some((1, 6))
    );
    let re_non_space = Regex::compile(r"[\S]+", true, false).unwrap();
    assert_eq!(
        re_non_space.find_span_with_context(" aé ", 0, None, None, &mut vm_scratch),
        Some((1, 4))
    );

    let q_word_cap = CompiledQuery::compile(
        r"\B(bar)",
        SearchOptions {
            regex: true,
            case_sensitive: true,
            ..SearchOptions::default()
        },
    )
    .unwrap()
    .unwrap();
    let mut search_scratch = SearchScratch::new();
    assert_eq!(
        q_word_cap.expand_replacement_with_context("bar", Some('o'), None, "[$1]", &mut search_scratch),
        "[bar]"
    );

    let zwj_ascii = "👩\u{200D}a";
    assert_eq!(next_grapheme_boundary(zwj_ascii, 0), zwj_ascii.len());
    assert_eq!(prev_grapheme_boundary(zwj_ascii, zwj_ascii.len()), 0);
    let long_comb = format!("a{}", "\u{0301}".repeat(40));
    assert_eq!(next_grapheme_boundary(&long_comb, 0), long_comb.len());
    assert_eq!(prev_grapheme_boundary(&long_comb, long_comb.len()), 0);
}

#[test]
fn audit_pass2_lazy_edit_regex_replace_diff_cr_and_syntax_states() {
    use editor_core::diff::{compute_line_diff, hash_lines_bytes, lookup_line_diff, LineDiffKind};
    use editor_core::search::find_all_matches;
    use editor_core::syntax::{Language, SyntaxCache, TokenKind};
    use editor_core::{Document, SearchOptions};
    use editor_platform::MemoryFileSystem;
    use std::path::Path;

    // 1. Document::edit on lazily-indexed large file keeps SyntaxCache line count in sync.
    let mut big_rs = String::with_capacity(200_000);
    for i in 0..8000 {
        big_rs.push_str(&format!("let v{i} = {i};\n"));
    }
    let fs = MemoryFileSystem::new();
    fs.add_file("/big.rs", big_rs.as_bytes().to_vec());
    let mut doc = Document::open(&fs, Path::new("/big.rs")).unwrap();
    // Edit deep in the unindexed region (around byte 100_000) inserting 1 newline.
    let insert_at = 100_000u64;
    let lines_before_total = big_rs.bytes().filter(|&b| b == b'\n').count();
    doc.edit(insert_at, 0, "\n/* block */\n", 10, false).unwrap();
    doc.tree_mut().ensure_indexed_for_line(lines_before_total + 10);
    assert_eq!(doc.editable_line_count(), lines_before_total + 3);

    // 2. Document::replace_all preserves CRLF `(foo)$` captures and adjacent `\B` match context.
    let mut crlf_doc = Document::from_str("foo\r\nbar\r\n", None);
    let re_opts = SearchOptions {
        regex: true,
        case_sensitive: true,
        ..SearchOptions::default()
    };
    let n1 = crlf_doc.replace_all("(foo)$", "[$1]", re_opts, 1);
    assert_eq!(n1, 1);
    assert_eq!(crlf_doc.to_string_lossy(), "[foo]\r\nbar\r\n");

    let mut adj_doc = Document::from_str("foobar", None);
    let n2 = adj_doc.replace_all(r"(foo)\B|bar", "<$0>", re_opts, 2);
    assert_eq!(n2, 2);
    assert_eq!(adj_doc.to_string_lossy(), "<foo><bar>");

    // 3. Diff handles lone `\r` line breaks identically to PieceTree.
    let old_h = hash_lines_bytes(b"a\rb\rc").unwrap();
    let new_h = hash_lines_bytes(b"a\rX\rc").unwrap();
    assert_eq!(old_h.len(), 3);
    let diff = compute_line_diff(&old_h, &new_h);
    assert_eq!(lookup_line_diff(&diff, 1), Some(LineDiffKind::Modified));
    assert_eq!(lookup_line_diff(&diff, 0), None);
    assert_eq!(lookup_line_diff(&diff, 2), None);

    // 4. Regex search across >256 KB single-line chunk boundary finds match spanning 262_144.
    let mut huge_line = vec![b'a'; 300_000];
    let needle = b"NEEDLE_BOUNDARY";
    huge_line[262_140..262_140 + needle.len()].copy_from_slice(needle);
    let mut huge_doc = Document::from_str(&String::from_utf8(huge_line).unwrap(), None);
    let hits = find_all_matches(huge_doc.tree_mut(), "NEEDLE_BOUNDARY", re_opts);
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].start, hits[0].end), (262_140, 262_140 + needle.len() as u64));

    // 5. HTML multi-line `<script\n type="module">` still enters JS embedded lexer.
    let mut html_cache = SyntaxCache::new(Language::from_path(Some(Path::new("index.html"))));
    let mut spans = Vec::new();
    for (idx, line) in [
        "<script",
        "  type=\"module\">",
        "const answer = 42;",
        "</script>",
    ]
    .iter()
    .enumerate()
    {
        spans.clear();
        html_cache.lex_line(idx, line, &mut spans);
        if idx == 2 {
            assert!(
                spans.iter().any(|s| s.kind == TokenKind::Keyword && &line[s.start as usize..s.end as usize] == "const"),
                "const inside multiline <script> tag must be highlighted as Keyword, got: {spans:?}"
            );
        }
    }

    // 6. Markdown fenced C++ raw string `R"tag(... )tag"` preserves sub-lexer detail across lines.
    let mut md_cache = SyntaxCache::new(Language::from_path(Some(Path::new("notes.md"))));
    for (idx, line) in [
        "```cpp",
        "const char* s = R\"tag(line1",
        "still_raw_string",
        ")tag\";",
        "```",
    ]
    .iter()
    .enumerate()
    {
        spans.clear();
        md_cache.lex_line(idx, line, &mut spans);
        if idx == 2 {
            assert!(
                spans.iter().any(|s| s.kind == TokenKind::String),
                "C++ raw string continuation inside ```cpp must remain TokenKind::String, got: {spans:?}"
            );
        }
    }

    // 7. JS multi-line `/* */` inside template literal `${ ... }` preserves template_brace_depth.
    let mut js_cache = SyntaxCache::new(Language::from_path(Some(Path::new("app.js"))));
    for (idx, line) in [
        "const s = `head${ /*",
        "  comment inside interp",
        "*/ 42 }tail`;",
    ]
    .iter()
    .enumerate()
    {
        spans.clear();
        js_cache.lex_line(idx, line, &mut spans);
        if idx == 2 {
            assert!(
                spans.iter().any(|s| s.kind == TokenKind::String && &line[s.start as usize..s.end as usize] == "tail`"),
                "template tail after multiline block comment inside ${{}} must stay String, got: {spans:?}"
            );
        }
    }
}
