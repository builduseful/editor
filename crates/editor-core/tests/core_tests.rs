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
    let mut cache = SyntaxCache::new(Language::Rust);
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
