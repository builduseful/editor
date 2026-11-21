//! Minimal, zero-dependency, deterministic JSON string builder.

pub struct JsonBuf {
    out: String,
}

impl JsonBuf {
    pub fn new() -> Self {
        Self {
            out: String::with_capacity(256),
        }
    }

    pub fn into_string(self) -> String {
        self.out
    }

    pub fn push_raw(&mut self, s: &str) {
        self.out.push_str(s);
    }

    pub fn push_str(&mut self, s: &str) {
        self.out.push('"');
        for ch in s.chars() {
            match ch {
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\\\"),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '\t' => self.out.push_str("\\t"),
                '\x08' => self.out.push_str("\\b"),
                '\x0C' => self.out.push_str("\\f"),
                c if (c as u32) < 0x20 => {
                    use std::fmt::Write;
                    let _ = write!(self.out, "\\u{:04x}", c as u32);
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    pub fn push_u64(&mut self, v: u64) {
        use std::fmt::Write;
        let _ = write!(self.out, "{v}");
    }

    pub fn push_usize(&mut self, v: usize) {
        use std::fmt::Write;
        let _ = write!(self.out, "{v}");
    }

    pub fn push_bool(&mut self, v: bool) {
        self.out.push_str(if v { "true" } else { "false" });
    }

    pub fn push_rev(&mut self, rev: editor_platform::FileRev) {
        use std::fmt::Write;
        let _ = write!(
            self.out,
            "{{\"dev\":\"{}\",\"ino\":\"{}\",\"size\":\"{}\",\"mtime_ns\":\"{}\",\"ctime_ns\":\"{}\"}}",
            rev.dev, rev.ino, rev.size, rev.mtime_ns, rev.ctime_ns
        );
    }
}
