//! `@rollup/plugin-replace`-compatible plain-text replacement, run over the
//! raw source before the pipeline parses it. Keys match verbatim — no regex,
//! no scope awareness, hits inside strings/comments/templates included —
//! leftmost-longest, at identifier boundaries; a `.` after the match also
//! kills it, so `typeof window` cannot hit inside `typeof window.document`.
//! Values splice verbatim with no padding, so positions survive only where
//! the splice is length-equal. One scan core serves both pipeline paths over
//! the [`Haystack`] abstraction (UTF-8 bytes on the String path, original
//! UTF-16 units on the lossless path — the lossy parse copy never
//! participates, so a key spelled U+FFFD cannot match a lone surrogate the
//! copy merely represents).

use std::collections::HashMap;

use oxc_syntax::identifier::{is_identifier_part, is_identifier_start};

/// The `replaceOptions` flags beyond the key→value map; both default false.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplaceOptions {
    /// Skip matches that look like an assignment (`KEY = x`, `KEY => x`) or
    /// a declaration (`const|let|var KEY`).
    pub prevent_assignment: bool,
    /// Derive `typeof prefix` guard keys for every legal member-chain key;
    /// the guard value is the quoted string literal `"object"`.
    pub object_guards: bool,
}

/// The raw `replace` configuration handed over the boundary: map plus flags.
pub struct ReplaceParams {
    pub entries: std::collections::HashMap<String, String>,
    pub prevent_assignment: bool,
    pub object_guards: bool,
}

/// One replacement: the key in both spellings the scan needs (the `String`
/// for bucket sizing, the UTF-16 units for element comparison), plus the
/// value spliced over the key.
#[derive(Debug)]
struct ReplaceEntry {
    key: String,
    key_units: Vec<u16>,
    value: String,
}

/// One scanned match: an element span in the haystack's own addressing plus
/// the table entry it came from. Spans are ascending and non-overlapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReplaceHit {
    start: u32,
    end: u32,
    entry: usize,
}

/// The lookup table: entries indexed by first-element buckets, each bucket
/// sorted longest-key-first, so the first textual hit in a bucket is the
/// leftmost-longest match.
#[derive(Debug)]
pub struct Replaces {
    entries: Vec<ReplaceEntry>,
    buckets: [Vec<u32>; 256],
    prevent_assignment: bool,
}

impl Replaces {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Validate every key (an empty key is rejected outright), derive the
    /// `objectGuards` keys, and bucket everything longest-first — the sort
    /// keeps bucket order and derived-key behavior deterministic regardless
    /// of the map's iteration order.
    pub(crate) fn new(
        entries: &HashMap<String, String>,
        options: ReplaceOptions,
    ) -> Result<Self, String> {
        let mut keys: Vec<&String> = entries.keys().collect();
        keys.sort();
        for key in &keys {
            if key.is_empty() {
                return Err(format!("invalid replace key {key:?}: must not be empty"));
            }
        }

        let mut replaces = Self {
            entries: Vec::new(),
            buckets: std::array::from_fn(|_| Vec::new()),
            prevent_assignment: options.prevent_assignment,
        };
        for key in &keys {
            replaces.insert(key.as_str(), &entries[key.as_str()]);
        }
        if options.object_guards {
            // `typeof a`, `typeof a.b` for `a.b.c` — every proper dotted
            // prefix, never the whole key. The value is the quoted string
            // literal `"object"` (rolldown's expand_typeof_replacements
            // spelling), never a bare `object` identifier. Only an explicit
            // `typeof a` user key suppresses the guard — a bare `a` key must
            // not, or `typeof a` would take the bare key's value.
            let mut guards: Vec<String> = keys
                .iter()
                .filter(|key| is_member_chain(key.as_str()))
                .flat_map(|key| dotted_prefixes(key))
                .map(|prefix| format!("typeof {prefix}"))
                .collect();
            guards.sort();
            guards.dedup();
            for guard in &guards {
                if !entries.contains_key(guard) {
                    replaces.insert(guard, "\"object\"");
                }
            }
        }
        for bucket in &mut replaces.buckets {
            let entries = &replaces.entries;
            bucket.sort_by_key(|&index| std::cmp::Reverse(entries[index as usize].key.len()));
        }
        Ok(replaces)
    }

    fn insert(&mut self, key: &str, value: &str) {
        let key_units: Vec<u16> = key.encode_utf16().collect();
        let index = self.entries.len() as u32;
        // the bucket is a pre-filter keyed by the first element masked to 256
        // slots; every candidate still gets a full element comparison
        let bucket = (key_units[0] as usize) & 0xFF;
        self.entries.push(ReplaceEntry {
            key: key.to_string(),
            key_units,
            value: value.to_string(),
        });
        self.buckets[bucket].push(index);
    }

    /// The scan core, written once over [`Haystack`] so the String path and
    /// the units path get the identical leftmost-longest behavior. Returns
    /// ascending, non-overlapping spans in the haystack's own addressing.
    fn scan_over<H: Haystack + ?Sized>(&self, src: &H) -> Vec<ReplaceHit> {
        let mut found = Vec::new();
        let mut at = 0usize;
        'outer: while at < src.size() {
            for &index in &self.buckets[src.bucket(at)] {
                let entry = &self.entries[index as usize];
                if let Some(end) = src.match_end(at, &entry.key_units)
                    && left_bounded(src, at)
                    && right_bounded(src, end)
                    && !self.assignment_blocked(src, at, end)
                {
                    found.push(ReplaceHit {
                        start: at as u32,
                        end: end as u32,
                        entry: index as usize,
                    });
                    at = end;
                    continue 'outer;
                }
                // a blocked candidate falls through to the next-shorter key at
                // the same position, like the reference plugins' regex
                // backtracking through its alternation
            }
            at = src.next_pos(at);
        }
        found
    }

    /// [`Self::scan_over`] over the String path's bytes.
    fn scan_hits(&self, src: &str) -> Vec<ReplaceHit> {
        self.scan_over(src)
    }

    /// [`Self::scan_hits`] mapped to the values — the shape the unit tests
    /// read.
    #[cfg(test)]
    fn scan<'a>(&'a self, src: &str) -> Vec<(u32, u32, &'a str)> {
        self.scan_hits(src)
            .into_iter()
            .map(|hit| (hit.start, hit.end, self.entries[hit.entry].value.as_str()))
            .collect()
    }

    /// Rewrite the String path's source: scan and splice each value verbatim
    /// over its match. With no hits the text is empty and the caller keeps
    /// `src`, so a match-free file pays neither a copy nor a second parse.
    pub(crate) fn rewrite(&self, src: &str) -> (String, usize) {
        let hits = self.scan_hits(src);
        if hits.is_empty() {
            return (String::new(), 0);
        }
        let mut out = String::with_capacity(src.len());
        let mut cursor = 0usize;
        for hit in &hits {
            let (start, end) = (hit.start as usize, hit.end as usize);
            out.push_str(&src[cursor..start]);
            out.push_str(&self.entries[hit.entry].value);
            cursor = end;
        }
        out.push_str(&src[cursor..]);
        (out, hits.len())
    }

    /// The lossless UTF-16 variant of [`Self::rewrite`]: the scan and the
    /// splice run on the original code units.
    pub(crate) fn rewrite_units(&self, units: &[u16]) -> (Vec<u16>, usize) {
        let hits = self.scan_over(units);
        if hits.is_empty() {
            return (Vec::new(), 0);
        }
        let mut out: Vec<u16> = Vec::with_capacity(units.len());
        let mut cursor = 0usize;
        for hit in &hits {
            let (start, end) = (hit.start as usize, hit.end as usize);
            out.extend_from_slice(&units[cursor..start]);
            out.extend(self.entries[hit.entry].value.encode_utf16());
            cursor = end;
        }
        out.extend_from_slice(&units[cursor..]);
        (out, hits.len())
    }

    /// The two `preventAssignment` skips, ported from the reference plugins:
    /// rolldown's declaration-prefix check (`const`/`let`/`var` + whitespace
    /// directly before the match), then the `(?!\s*=[^=])` lookahead — which
    /// blocks `KEY = x` and `KEY => x` but leaves `KEY == x`/`KEY === x` and
    /// `KEY += x` alone, the same footgun the reference plugins have.
    fn assignment_blocked<H: Haystack + ?Sized>(&self, src: &H, start: usize, end: usize) -> bool {
        if !self.prevent_assignment {
            return false;
        }
        if preceded_by_declaration(src, start) {
            return true;
        }
        let mut cursor = end;
        while cursor < src.size() && src.at(cursor).is_some_and(is_js_whitespace) {
            cursor = src.next_pos(cursor);
        }
        if cursor < src.size() && src.at(cursor) == Some('=' as u32) {
            let after = src.next_pos(cursor);
            if after < src.size() && src.at(after) != Some('=' as u32) {
                return true;
            }
        }
        false
    }
}

/// The text a scan runs over, addressed in the scan's own element: UTF-8
/// bytes on the String path, UTF-16 code units on the lossless path. Every
/// matching decision lives in the one [`Replaces::scan_over`] loop.
trait Haystack {
    /// Number of elements.
    fn size(&self) -> usize;
    /// The code point (String path) or code unit (units path) at `at`, or
    /// `None` past the end. Callers only address element boundaries.
    fn at(&self, at: usize) -> Option<u32>;
    /// The bucket pre-filter slot for the element at `at` — the first byte
    /// when ASCII, otherwise the first UTF-16 unit of the element, masked
    /// the same way as the insert side.
    fn bucket(&self, at: usize) -> usize;
    /// Where a match of `key` (as UTF-16 units) starting at `at` would end,
    /// or `None` when the elements differ.
    fn match_end(&self, at: usize, key: &[u16]) -> Option<usize>;
    /// Whether the elements before `at` end with `key`.
    fn ends_with(&self, at: usize, key: &[u16]) -> bool;
    /// The element boundary strictly before `at`.
    fn prev_pos(&self, at: usize) -> usize;
    /// The next scan position at or after `at + 1`.
    fn next_pos(&self, at: usize) -> usize;
}

impl Haystack for str {
    fn size(&self) -> usize {
        self.len()
    }

    fn at(&self, at: usize) -> Option<u32> {
        self.get(at..)
            .and_then(|tail| tail.chars().next())
            .map(|c| c as u32)
    }

    fn bucket(&self, at: usize) -> usize {
        let first = self.as_bytes()[at];
        if first < 0x80 {
            return first as usize;
        }
        // non-ASCII: the first UTF-16 unit of the char (its code point, or
        // the high surrogate half when astral), so astral-anchored keys probe
        // the bucket they were filed under on both pipeline paths
        let unit = self
            .get(at..)
            .and_then(|tail| tail.chars().next())
            .map_or(0, |c| {
                let mut buf = [0u16; 2];
                c.encode_utf16(&mut buf)[0]
            });
        (unit as usize) & 0xFF
    }

    fn match_end(&self, at: usize, key: &[u16]) -> Option<usize> {
        // compare in units: an astral char matches only a key that spells
        // the same surrogate pair
        let mut cursor = at;
        let mut key_at = 0usize;
        while key_at < key.len() {
            let c = self.get(cursor..)?.chars().next()?;
            let mut buf = [0u16; 2];
            let units = c.encode_utf16(&mut buf);
            if key.len() - key_at < units.len() || key[key_at..key_at + units.len()] != *units {
                return None;
            }
            key_at += units.len();
            cursor += c.len_utf8();
        }
        Some(cursor)
    }

    fn ends_with(&self, at: usize, key: &[u16]) -> bool {
        ends_with_elements(self, at, key)
    }

    fn prev_pos(&self, at: usize) -> usize {
        let mut prev = at - 1;
        while !self.is_char_boundary(prev) {
            prev -= 1;
        }
        prev
    }

    fn next_pos(&self, at: usize) -> usize {
        let mut next = at + 1;
        while next < self.len() && !self.is_char_boundary(next) {
            next += 1;
        }
        next
    }
}

impl Haystack for [u16] {
    fn size(&self) -> usize {
        self.len()
    }

    fn at(&self, at: usize) -> Option<u32> {
        self.get(at).map(|&unit| unit as u32)
    }

    fn bucket(&self, at: usize) -> usize {
        (self[at] as usize) & 0xFF
    }

    fn match_end(&self, at: usize, key: &[u16]) -> Option<usize> {
        let end = at + key.len();
        if end <= self.len() && &self[at..end] == key {
            Some(end)
        } else {
            None
        }
    }

    fn ends_with(&self, at: usize, key: &[u16]) -> bool {
        ends_with_elements(self, at, key)
    }

    fn prev_pos(&self, at: usize) -> usize {
        at - 1
    }

    fn next_pos(&self, at: usize) -> usize {
        at + 1
    }
}

/// Shared backward comparison for the declaration-prefix check. Only ever
/// called with ASCII keywords, whose units equal their bytes on both paths.
fn ends_with_elements<H: Haystack + ?Sized>(src: &H, at: usize, key: &[u16]) -> bool {
    if at < key.len() {
        return false;
    }
    let mut cursor = at;
    for &unit in key.iter().rev() {
        cursor = src.prev_pos(cursor);
        if src.at(cursor) != Some(unit as u32) {
            return false;
        }
    }
    true
}

/// The left delimiter: the match must not be the tail of a longer word.
fn left_bounded<H: Haystack + ?Sized>(src: &H, at: usize) -> bool {
    at == 0 || !src.at(src.prev_pos(at)).is_some_and(is_word_element)
}

/// The right delimiter: the next element must be outside the identifier
/// class and not a `.` — the member-access dot would make the hit a prefix
/// of a longer chain (`typeof window` in `typeof window.document`).
fn right_bounded<H: Haystack + ?Sized>(src: &H, end: usize) -> bool {
    match src.at(end) {
        None => true,
        Some(c) => !is_word_element(c) && c != '.' as u32,
    }
}

/// The UTF-16 units of an ASCII keyword.
const fn ascii_units<const N: usize>(bytes: &[u8; N]) -> [u16; N] {
    let mut units = [0u16; N];
    let mut at = 0;
    while at < N {
        units[at] = bytes[at] as u16;
        at += 1;
    }
    units
}

/// The declaration keywords' UTF-16 spellings as static slices.
const DECLARATION_KEYWORDS: [&[u16]; 3] = [
    &ascii_units(b"const"),
    &ascii_units(b"let"),
    &ascii_units(b"var"),
];

/// Whether the only text between the last non-whitespace element and the
/// match is a whole `const`/`let`/`var` keyword.
fn preceded_by_declaration<H: Haystack + ?Sized>(src: &H, start: usize) -> bool {
    let mut cursor = start;
    let mut saw_space = false;
    while cursor > 0 {
        let prev = src.prev_pos(cursor);
        match src.at(prev) {
            Some(c) if is_js_whitespace(c) => {
                saw_space = true;
                cursor = prev;
            }
            _ => break,
        }
    }
    if !saw_space {
        return false;
    }
    for keyword in DECLARATION_KEYWORDS {
        if src.ends_with(cursor, keyword) {
            let before = cursor - keyword.len();
            if before == 0 || !src.at(src.prev_pos(before)).is_some_and(is_word_element) {
                return true;
            }
        }
    }
    false
}

/// The identifier-boundary class `[_$A-Za-z0-9\xA0-\uFFFF]`, judged per
/// element — on the units path every code unit ≥ U+A0 (lone surrogates
/// included) reads as part of a longer word, like the reference plugins'
/// unit-wise class.
fn is_word_element(element: u32) -> bool {
    element == '_' as u32
        || element == '$' as u32
        || (element < 0x80 && (element as u8).is_ascii_alphanumeric())
        || element >= 0xA0
}

/// The regex `\s` class exactly as ECMAScript defines it (U+FEFF included)
/// — deliberately not Rust's `White_Space`, which disagrees with the js
/// class in both directions (no U+FEFF, but a stray U+0085).
fn is_js_whitespace(element: u32) -> bool {
    matches!(
        element,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000
            ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

/// Whether `segment` is spelled like an identifier, using the parser's own
/// tables (`ID_Start`/`ID_Continue` plus `$`, `_`, ZWNJ and ZWJ) — so the
/// guard derivation never disagrees with the key's own replaceability.
fn is_identifier_spelling(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(first) if is_identifier_start(first) => {}
        _ => return false,
    }
    chars.all(is_identifier_part)
}

/// Whether `key` is a legal member chain — at least two dot-separated
/// segments, every segment nonempty and spelled like an identifier; `a.b()`,
/// `a-b.c`, `a..b` and single identifiers derive nothing.
fn is_member_chain(key: &str) -> bool {
    let mut segments = 0usize;
    for segment in key.split('.') {
        if !is_identifier_spelling(segment) {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

/// Every proper dotted prefix of `key` (`a.b.c` → `a`, `a.b`).
fn dotted_prefixes(key: &str) -> Vec<&str> {
    let mut prefixes = Vec::new();
    let mut offset = 0;
    while let Some(dot) = key[offset..].find('.') {
        let end = offset + dot;
        if end > 0 {
            prefixes.push(&key[..end]);
        }
        offset = end + 1;
    }
    prefixes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(entries: &[(&str, &str)], src: &str) -> Vec<(u32, u32, String)> {
        scan_with(entries, src, ReplaceOptions::default())
    }

    fn scan_with(
        entries: &[(&str, &str)],
        src: &str,
        options: ReplaceOptions,
    ) -> Vec<(u32, u32, String)> {
        let map: HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let replaces = Replaces::new(&map, options).expect("valid entries");
        replaces
            .scan_hits(src)
            .into_iter()
            .map(|hit| {
                let value = replaces.entries[hit.entry].value.clone();
                (hit.start, hit.end, value)
            })
            .collect()
    }

    /// [`Replaces::rewrite`] — the pure-text splice.
    fn rewrite(entries: &[(&str, &str)], src: &str) -> (String, usize) {
        rewrite_with(entries, src, ReplaceOptions::default())
    }

    fn rewrite_with(
        entries: &[(&str, &str)],
        src: &str,
        options: ReplaceOptions,
    ) -> (String, usize) {
        let map: HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Replaces::new(&map, options)
            .expect("valid entries")
            .rewrite(src)
    }

    /// [`Replaces::rewrite_units`] against the original units of `src`, with
    /// the raw lone surrogate at `surrogate_at` swapped in (0 = none).
    fn rewrite_units(
        entries: &[(&str, &str)],
        src: &str,
        surrogate_at: usize,
    ) -> (Vec<u16>, usize) {
        let mut units: Vec<u16> = src.encode_utf16().collect();
        if surrogate_at > 0 {
            units[surrogate_at] = 0xD800;
        }
        let map: HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Replaces::new(&map, ReplaceOptions::default())
            .expect("valid entries")
            .rewrite_units(&units)
    }

    #[test]
    fn scans_leftmost_longest_without_overlap() {
        assert_eq!(scan(&[("KEY", "v")], "0 KEY 0"), [(2, 5, "v".into())]);
        assert_eq!(
            scan(&[("KEY", "v")], "KEY KEY"),
            [(0, 3, "v".into()), (4, 7, "v".into())]
        );
        assert_eq!(scan(&[("KEY", "v")], "xKEYx"), vec![]);
        assert_eq!(scan(&[("KEY", "v")], "_KEY"), vec![]);
        assert_eq!(scan(&[("KEY", "v")], "$KEY"), vec![]);
        assert_eq!(scan(&[("KEY", "v")], "KEY$"), vec![]);
        assert_eq!(
            scan(&[("a", "1"), ("a.b", "2"), ("a.b.c", "3")], "a.b.c"),
            [(0, 5, "3".into())]
        );
        assert_eq!(
            scan(&[("a", "1"), ("a.b", "2")], "a.b a"),
            [(0, 3, "2".into()), (4, 5, "1".into())]
        );
    }

    #[test]
    fn right_boundary_kills_a_member_dot() {
        assert_eq!(scan(&[("a", "1")], "a.b"), vec![]);
        assert_eq!(scan(&[("a", "1")], "a + b"), [(0, 1, "1".into())]);
        // only a plain `.` kills the match: the `?` of `?.` is not one
        assert_eq!(scan(&[("a", "1")], "a?.b"), [(0, 1, "1".into())]);
    }

    #[test]
    fn unicode_bounds_judge_per_char() {
        // é, λ, 变 and non-BMP emoji are all >= \xA0: inside the boundary
        // class, so the key reads as part of a longer word
        assert_eq!(scan(&[("KEY", "v")], "éKEY"), vec![]);
        assert_eq!(scan(&[("KEY", "v")], "λKEY"), vec![]);
        assert_eq!(scan(&[("KEY", "v")], "变KEY"), vec![]);
        assert_eq!(scan(&[("KEY", "v")], "\u{1F600}KEY"), vec![]);
        // a C1 control (U+0080..=U+009F) is non-ASCII yet outside the class —
        // the per-char judgment a first-byte test cannot express
        assert_eq!(scan(&[("KEY", "v")], "\u{9F}KEY"), [(2, 5, "v".into())]);
    }

    #[test]
    fn non_ascii_keys_probe_their_own_bucket_on_both_paths() {
        // the bucket pre-filter is the masked first UTF-16 element, so a
        // CJK- or astral-anchored key matches through the byte path too
        assert_eq!(scan(&[("变量", "1")], "变量 + x"), [(0, 6, "1".into())]);
        assert_eq!(
            scan(&[("\u{1F680}x", "1")], "\u{1F680}x"),
            [(0, 5, "1".into())]
        );
    }

    #[test]
    fn rewrite_splices_every_hit_exactly() {
        // pure text: code, comments, string interiors and a hit crossing any
        // token shape all splice the value verbatim
        let (rewritten, hits) = rewrite(&[("FLAG", "1")], "log(FLAG)");
        assert_eq!(hits, 1);
        assert_eq!(rewritten, "log(1)");

        let (rewritten, _) = rewrite(&[("FLAG", "λ")], "log(FLAG)");
        assert_eq!(rewritten, "log(λ)");

        let (rewritten, _) = rewrite(&[("FLAG", "longervalue")], "log(FLAG)");
        assert_eq!(rewritten, "log(longervalue)");

        let (rewritten, _) = rewrite(&[("FLAG", "")], "log(FLAG)");
        assert_eq!(rewritten, "log()");

        let (rewritten, _) = rewrite(&[("变量", "1")], "log(变量)");
        assert_eq!(rewritten, "log(1)");

        let (rewritten, _) = rewrite(&[("FLAG", "1")], "/* FLAG */");
        assert_eq!(rewritten, "/* 1 */");
        let (rewritten, _) = rewrite(&[("FLAG", "1")], "const s = \"FLAG\";");
        assert_eq!(rewritten, "const s = \"1\";");
        let (rewritten, _) = rewrite(&[("FLAG", "1")], "const t = `${FLAG}`;");
        assert_eq!(rewritten, "const t = `${1}`;");

        let (rewritten, hits) = rewrite(&[("f(\"FLAG", "f(\"X")], "f(\"FLAG\");");
        assert_eq!(hits, 1);
        assert_eq!(rewritten, "f(\"X\");");
    }

    #[test]
    fn rewrite_splices_a_user_padded_tail_verbatim() {
        // trailing spaces are part of the value and splice verbatim
        let (rewritten, hits) = rewrite(&[("FLAG", "1   ")], "log(FLAG);");
        assert_eq!(hits, 1);
        assert_eq!(rewritten, "log(1   );");
        assert_eq!(rewritten.len(), "log(FLAG);".len());
    }

    #[test]
    fn rewrite_with_zero_hits_returns_no_text() {
        // the caller keeps the original on zero hits
        let (rewritten, hits) = rewrite(&[("FLAG", "1")], "log(KEY)");
        assert_eq!(hits, 0);
        assert_eq!(rewritten, "");
    }

    #[test]
    fn rewrite_units_splices_exactly() {
        let (rewritten, hits) = rewrite_units(&[("FLAG", "1")], "log(FLAG)", 0);
        assert_eq!(hits, 1);
        assert_eq!(
            String::from_utf16(&rewritten).expect("valid units"),
            "log(1)"
        );

        // a raw lone surrogate never answers a U+FFFD key: the units
        // comparison sees 0xD800, so the phantom match is structurally
        // impossible
        let (rewritten, hits) = rewrite_units(&[("\u{FFFD}", "X")], "log(a\u{FFFD}b)", 5);
        assert_eq!(hits, 0);
        assert_eq!(rewritten, Vec::<u16>::new());

        // a genuine U+FFFD unit in the same file still replaces
        let (rewritten, hits) = rewrite_units(&[("\u{FFFD}", "X")], "log(1 + \u{FFFD})", 0);
        assert_eq!(hits, 1);
        assert_eq!(
            String::from_utf16(&rewritten).expect("valid units"),
            "log(1 + X)"
        );

        let (rewritten, hits) = rewrite_units(&[("FLAG", "1")], "const s = \"FLAG\";", 0);
        assert_eq!(hits, 1);
        assert_eq!(
            String::from_utf16(&rewritten).expect("valid units"),
            "const s = \"1\";"
        );
    }

    #[test]
    fn rewrite_units_probes_the_masked_bucket_for_an_astral_key() {
        let src = "const s = \"\u{1F680}x\";";
        let units: Vec<u16> = src.encode_utf16().collect();
        let map: HashMap<String, String> = [("\u{1F680}x".to_string(), "Y".to_string())]
            .into_iter()
            .collect();
        let (rewritten, hits) = Replaces::new(&map, ReplaceOptions::default())
            .expect("valid entries")
            .rewrite_units(&units);
        assert_eq!(hits, 1);
        assert_eq!(
            String::from_utf16(&rewritten).expect("valid units"),
            "const s = \"Y\";"
        );
    }

    #[test]
    fn prevent_assignment_blocks_lookahead_shapes() {
        let options = ReplaceOptions {
            prevent_assignment: true,
            object_guards: false,
        };
        assert_eq!(scan_with(&[("KEY", "v")], "KEY = 1", options), vec![]);
        assert_eq!(scan_with(&[("KEY", "v")], "KEY => 1", options), vec![]);
        assert_eq!(scan_with(&[("KEY", "v")], "KEY\t= 1", options), vec![]);
        // not blocked: comparisons and compound assignment — the reference
        // plugins' own lookahead `(?!\s*=[^=])` passes those
        assert_eq!(
            scan_with(&[("KEY", "v")], "KEY == 1", options),
            [(0, 3, "v".into())]
        );
        assert_eq!(
            scan_with(&[("KEY", "v")], "KEY === 1", options),
            [(0, 3, "v".into())]
        );
        assert_eq!(
            scan_with(&[("KEY", "v")], "KEY += 1", options),
            [(0, 3, "v".into())]
        );
        // the whitespace class is the js regex `\s`, not rust's
        // `White_Space`: U+FEFF and U+00A0 join the run before the `=` and
        // block, while U+0085 is not js whitespace and never joins
        assert_eq!(
            scan_with(&[("KEY", "v")], "KEY \u{FEFF}= 1", options),
            vec![]
        );
        assert_eq!(
            scan_with(&[("KEY", "v")], "KEY\u{FEFF}= 1", options),
            vec![]
        );
        assert_eq!(scan_with(&[("KEY", "v")], "KEY\u{A0}= 1", options), vec![]);
        assert_eq!(
            scan_with(&[("KEY", "v")], "KEY\u{85}= 1", options),
            [(0, 3, "v".into())]
        );
        // a trailing `=` at EOF has no `[^=]` char behind it: not blocked
        assert_eq!(
            scan_with(&[("KEY", "v")], "KEY =", options),
            [(0, 3, "v".into())]
        );
        assert_eq!(
            scan_with(&[("KEY", "v")], "KEY", options),
            [(0, 3, "v".into())]
        );
        // off by default: the same shapes replace
        assert_eq!(scan(&[("KEY", "v")], "KEY = 1"), [(0, 3, "v".into())]);
    }

    #[test]
    fn prevent_assignment_blocks_declaration_prefixes() {
        let options = ReplaceOptions {
            prevent_assignment: true,
            object_guards: false,
        };
        assert_eq!(scan_with(&[("KEY", "v")], "const KEY = 1", options), vec![]);
        assert_eq!(scan_with(&[("KEY", "v")], "let KEY", options), vec![]);
        assert_eq!(scan_with(&[("KEY", "v")], "var\nKEY", options), vec![]);
        assert_eq!(
            scan_with(&[("KEY", "v")], "const \u{FEFF} KEY", options),
            vec![]
        );
        // only a whole keyword: a longer word ending in one is a plain read
        assert_eq!(
            scan_with(&[("KEY", "v")], "xconst KEY", options),
            [(7, 10, "v".into())]
        );
        // the keyword `const` itself can be a key
        assert_eq!(
            scan_with(&[("const", "v")], "x const", options),
            [(2, 7, "v".into())]
        );
    }

    #[test]
    fn object_guards_derive_typeof_prefixes() {
        let options = ReplaceOptions {
            prevent_assignment: false,
            object_guards: true,
        };
        assert_eq!(
            scan_with(&[("a.b.c", "X")], "typeof a", options),
            [(0, 8, "\"object\"".into())]
        );
        // a combining-mark chain is a legal identifier (ID_Continue)
        assert_eq!(
            scan_with(&[("e\u{0301}.x", "X")], "typeof e\u{0301}", options),
            [(0, 10, "\"object\"".into())]
        );
        assert_eq!(
            scan_with(&[("a.b.c", "X")], "typeof a.b", options),
            [(0, 10, "\"object\"".into())]
        );
        // the whole key is not a guard
        assert_eq!(
            scan_with(&[("a.b.c", "X")], "typeof a.b.c.d", options),
            vec![]
        );
        // guard keys match literally: two spaces miss, like rollup
        assert_eq!(scan_with(&[("a.b", "X")], "typeof  a", options), vec![]);
        // off: no derived keys exist
        assert_eq!(scan(&[("a.b", "X")], "typeof a"), vec![]);
    }

    #[test]
    fn object_guard_user_keys_win_over_derived() {
        let options = ReplaceOptions {
            prevent_assignment: false,
            object_guards: true,
        };
        assert_eq!(
            scan_with(&[("a.b", "X"), ("typeof a", "U")], "typeof a", options),
            [(0, 8, "U".into())]
        );
    }

    #[test]
    fn object_guard_conflict_reads_the_full_derived_key() {
        let options = ReplaceOptions {
            prevent_assignment: false,
            object_guards: true,
        };
        // `a` is a user key but `typeof a` is not: the `a.b` guard must
        // still derive, or `typeof a` would take the bare key's value
        assert_eq!(
            scan_with(&[("a", "X"), ("a.b", "Y")], "typeof a", options),
            [(0, 8, "\"object\"".into())]
        );
        assert_eq!(
            scan_with(&[("a", "X"), ("a.b", "Y")], "a;", options),
            [(0, 1, "X".into())]
        );
        // an explicitly spelled `typeof a` key still suppresses the guard
        assert_eq!(
            scan_with(&[("a.b", "Y"), ("typeof a", "T")], "typeof a", options),
            [(0, 8, "T".into())]
        );
    }

    #[test]
    fn object_guards_derive_only_for_legal_member_chains() {
        let options = ReplaceOptions {
            prevent_assignment: false,
            object_guards: true,
        };
        let guards = |key: &str| {
            let map: HashMap<String, String> =
                [(key.to_string(), "X".to_string())].into_iter().collect();
            Replaces::new(&map, options)
                .expect("valid entries")
                .entries
                .iter()
                .map(|entry| entry.key.as_str())
                .filter(|key| key.starts_with("typeof "))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        // a legal chain derives every proper prefix
        assert_eq!(guards("a.b.c"), ["typeof a", "typeof a.b"]);
        assert_eq!(
            guards("process.env.NODE_ENV"),
            ["typeof process", "typeof process.env"]
        );
        assert_eq!(guards("变量.env"), ["typeof 变量"]);
        // ID_Continue reaches past letters and digits: combining marks and
        // the joiners continue a segment ...
        assert_eq!(guards("e\u{0301}.x"), ["typeof e\u{0301}"]);
        assert_eq!(guards("a\u{200C}b.c"), ["typeof a\u{200C}b"]);
        assert_eq!(guards("x\u{200D}y.z"), ["typeof x\u{200D}y"]);
        // ... but only continue: a segment starting with one is no
        // identifier at all
        assert!(guards("\u{200C}a.b").is_empty());
        assert!(guards("a.\u{200D}b").is_empty());
        // single segments, call shapes, operators and empty segments derive
        // nothing
        assert!(guards("a").is_empty());
        assert!(guards("a.b()").is_empty());
        assert!(guards("a-b.c").is_empty());
        assert!(guards("a..b").is_empty());
        assert!(guards("a.b.").is_empty());
        assert!(guards(".a.b").is_empty());
        assert!(guards("a.1b").is_empty());
    }

    #[test]
    fn object_guard_derivation_dedupes_across_keys() {
        let options = ReplaceOptions {
            prevent_assignment: false,
            object_guards: true,
        };
        let map: HashMap<String, String> = [
            ("process.env.A".to_string(), "1".to_string()),
            ("process.env.B".to_string(), "2".to_string()),
            ("process.versions.node".to_string(), "3".to_string()),
        ]
        .into_iter()
        .collect();
        let replaces = Replaces::new(&map, options).expect("valid entries");
        // 3 user keys + one guard per distinct prefix — never one guard set
        // per key
        assert_eq!(replaces.entries.len(), 6);
        let guard_keys = replaces
            .entries
            .iter()
            .map(|entry| entry.key.as_str())
            .filter(|key| key.starts_with("typeof "))
            .collect::<Vec<_>>();
        assert_eq!(
            guard_keys,
            [
                "typeof process",
                "typeof process.env",
                "typeof process.versions"
            ]
        );
    }

    #[test]
    fn rejects_empty_keys() {
        let map: HashMap<String, String> = ["".to_string(), "x".to_string()]
            .into_iter()
            .map(|k| (k, "v".to_string()))
            .collect();
        let error = Replaces::new(&map, ReplaceOptions::default()).unwrap_err();
        assert_eq!(error, "invalid replace key \"\": must not be empty");
    }

    #[test]
    fn buckets_filter_by_first_byte() {
        let replaces = Replaces::new(
            &HashMap::from([("z".to_string(), "1".to_string())]),
            ReplaceOptions::default(),
        )
        .unwrap();
        assert_eq!(
            replaces
                .scan("z z a")
                .into_iter()
                .map(|(start, end, _)| (start, end))
                .collect::<Vec<_>>(),
            [(0, 1), (2, 3)]
        );
        assert!(replaces.scan_hits("abc").is_empty());
        // bucket order is longest-first: the scan tries `ab` before `a`
        let replaces = Replaces::new(
            &HashMap::from([
                ("a".to_string(), "1".to_string()),
                ("ab".to_string(), "2".to_string()),
            ]),
            ReplaceOptions::default(),
        )
        .unwrap();
        assert_eq!(
            replaces
                .scan_hits("ab")
                .into_iter()
                .map(|hit| (hit.start, hit.end))
                .collect::<Vec<_>>(),
            [(0, 2)]
        );
    }
}
