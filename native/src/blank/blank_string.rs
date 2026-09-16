const REPLACE_WITH_BLANK: u8 = 0;
const REPLACE_WITH_OPEN_PAREN: u8 = 1;
const REPLACE_WITH_CLOSE_PAREN: u8 = 2;
const REPLACE_WITH_SEMI: u8 = 3;
const REPLACE_WITH_TEXT: u8 = 4;
/// Modifier bit OR-ed onto [`REPLACE_WITH_BLANK`]/[`REPLACE_WITH_SEMI`]:
/// the blank also keeps U+2028/U+2029 as line breaks. The dce hollow sets
/// it — its edits reach plain JavaScript no earlier pass rewrote, so its
/// position promise must hold for all four JavaScript line terminators.
const KEEP_LINE_BREAKS: u8 = 0b1000_0000;

/// An override text: kept as the string it came from on the String path,
/// and encoded only for the lossless UTF-16 path — a Rust `String` can
/// never hold raw lone surrogates, so only the units caller needs that
/// form, and `build` skips a UTF-16 round trip for every splice.
enum SpliceText {
    Str(String),
    Units(Vec<u16>),
}

/// Like magic-string, restricted to two features: blanking ranges (newlines
/// preserved so line/column stay stable) and literal overwrites (enum
/// expansion, grouping parens).
#[derive(Default)]
pub struct BlankString {
    /// Flat (flags, start, end, textIndex) tuples, pushed in source order.
    ranges: Vec<(u8, u32, u32, u32)>,
    texts: Vec<SpliceText>,
}

impl BlankString {
    /// Replace [start, end) with `text`; `end` may equal `start` to insert.
    pub fn override_range(&mut self, start: u32, end: u32, text: impl AsRef<str>) {
        let index = self.texts.len() as u32;
        self.texts.push(SpliceText::Str(text.as_ref().to_string()));
        self.ranges.push((REPLACE_WITH_TEXT, start, end, index));
    }

    /// [`override_range`](Self::override_range) with the replacement given as
    /// UTF-16 units — required when the text contains raw lone surrogates.
    pub fn override_range_units(&mut self, start: u32, end: u32, prefix: &str, units: &[u16]) {
        let index = self.texts.len() as u32;
        let mut text: Vec<u16> = prefix.encode_utf16().collect();
        text.extend_from_slice(units);
        self.texts.push(SpliceText::Units(text));
        self.ranges.push((REPLACE_WITH_TEXT, start, end, index));
    }

    /// [`override_range`](Self::override_range) spliced in at its source
    /// position among the pushed ranges: the enum walk erases an initializer
    /// first and collects reference rewrites second, and out-of-order ranges
    /// would corrupt [`build`](Self::build).
    pub fn override_range_sorted(&mut self, start: u32, end: u32, text: String) {
        let index = self.texts.len() as u32;
        self.texts.push(SpliceText::Str(text));
        let at = self
            .ranges
            .partition_point(|&(_, range_start, _, _)| range_start <= start);
        self.ranges
            .insert(at, (REPLACE_WITH_TEXT, start, end, index));
    }

    /// Whether [start, end) overlaps an already-pushed range: an identifier
    /// inside an erased region (a type alias, an annotation) must not be
    /// rewritten — splicing text into erased content corrupts the output.
    pub fn overlaps_pushed_range(&self, start: u32, end: u32) -> bool {
        let pushed = self
            .ranges
            .partition_point(|&(_, range_start, _, _)| range_start < end);
        pushed > 0 && self.ranges[pushed - 1].2 > start
    }

    pub fn blank_but_start_with_open_paren(&mut self, start: u32, end: u32) {
        self.ranges
            .push((REPLACE_WITH_OPEN_PAREN, start, end, u32::MAX));
    }

    pub fn blank_but_end_with_close_paren(&mut self, start: u32, end: u32) {
        self.ranges
            .push((REPLACE_WITH_BLANK, start, end - 1, u32::MAX));
        self.ranges
            .push((REPLACE_WITH_CLOSE_PAREN, end - 1, end, u32::MAX));
    }

    pub fn blank_but_start_with_semi(&mut self, start: u32, end: u32) {
        self.ranges.push((REPLACE_WITH_SEMI, start, end, u32::MAX));
    }

    pub fn blank(&mut self, start: u32, end: u32) {
        self.ranges.push((REPLACE_WITH_BLANK, start, end, u32::MAX));
    }

    /// [`blank`](Self::blank) keeping U+2028/U+2029 as line breaks too —
    /// see [`KEEP_LINE_BREAKS`].
    pub fn blank_keep_line_breaks(&mut self, start: u32, end: u32) {
        self.ranges
            .push((REPLACE_WITH_BLANK | KEEP_LINE_BREAKS, start, end, u32::MAX));
    }

    /// [`blank_but_start_with_semi`](Self::blank_but_start_with_semi)
    /// keeping U+2028/U+2029 as line breaks too — see [`KEEP_LINE_BREAKS`].
    pub fn blank_but_start_with_semi_keep_line_breaks(&mut self, start: u32, end: u32) {
        self.ranges
            .push((REPLACE_WITH_SEMI | KEEP_LINE_BREAKS, start, end, u32::MAX));
    }

    /// Splice into one exactly-sized buffer; the capacity for text overrides
    /// is computed up front so the whole output is a single allocation.
    pub fn build(&self, input: &str) -> String {
        let ranges = &self.ranges;
        if ranges.is_empty() {
            return input.to_string();
        }

        let mut extra = 0usize;
        for &(flags, start, end, text_index) in ranges {
            if flags == REPLACE_WITH_TEXT {
                let len = match &self.texts[text_index as usize] {
                    SpliceText::Str(text) => text.len(),
                    SpliceText::Units(units) => units.len(),
                };
                extra += len.saturating_sub((end - start) as usize);
            }
        }

        let mut out = Vec::with_capacity(input.len() + extra);
        let mut previous_end = 0u32;

        for &(raw_flags, start, end, text_index) in ranges {
            let keep_line_breaks = raw_flags & KEEP_LINE_BREAKS != 0;
            let flags = raw_flags & !KEEP_LINE_BREAKS;
            let range_start = start.max(previous_end);
            out.extend_from_slice(&input.as_bytes()[previous_end as usize..range_start as usize]);

            let mut range_start = range_start;
            match flags {
                REPLACE_WITH_TEXT => match &self.texts[text_index as usize] {
                    SpliceText::Str(text) => out.extend_from_slice(text.as_bytes()),
                    SpliceText::Units(units) => out.extend_from_slice(
                        String::from_utf16(units)
                            .expect("String-path texts are valid UTF-16")
                            .as_bytes(),
                    ),
                },
                REPLACE_WITH_CLOSE_PAREN => {
                    out.push(b')');
                    range_start += 1;
                }
                REPLACE_WITH_SEMI => {
                    out.push(b';');
                    range_start += 1;
                }
                REPLACE_WITH_OPEN_PAREN => {
                    out.push(b'(');
                    range_start += 1;
                }
                _ => {}
            }

            previous_end = end;
            if flags != REPLACE_WITH_TEXT {
                write_space(&mut out, input, range_start, previous_end, keep_line_breaks);
            }
        }

        out.extend_from_slice(&input.as_bytes()[previous_end as usize..]);
        // the buffer only ever holds input bytes, spaces, and caller text
        String::from_utf8(out).expect("output buffer is valid UTF-8")
    }

    /// [`build`](Self::build) consuming the input buffer: on the common file
    /// whose edits are all same-length overwrites, the input itself is mutated
    /// in place and handed back, skipping the fresh allocation and full copy.
    /// Text splices lengthen the output, a blanked non-ASCII char shrinks to
    /// its UTF-16 width (`len_utf16` spaces for `len_utf8` bytes), and a
    /// paren/semi marker on an empty range grows by one byte — none of those
    /// can happen in place, so any of them falls back to [`build`](Self::build).
    pub fn build_owned(self, input: String) -> String {
        let bytes = input.as_bytes();
        let mut covered_end = 0u32;
        let in_place = self.ranges.iter().all(|&(raw_flags, start, end, _)| {
            let flags = raw_flags & !KEEP_LINE_BREAKS;
            let ok = flags != REPLACE_WITH_TEXT
                && start >= covered_end
                && (flags == REPLACE_WITH_BLANK || end > start)
                && bytes[start as usize..end as usize].is_ascii();
            covered_end = covered_end.max(end);
            ok
        });
        if !in_place {
            return self.build(&input);
        }

        let mut buffer = input.into_bytes();
        for &(raw_flags, start, end, _) in &self.ranges {
            let flags = raw_flags & !KEEP_LINE_BREAKS;
            let mut at = start as usize;
            if flags != REPLACE_WITH_BLANK {
                buffer[at] = match flags {
                    REPLACE_WITH_OPEN_PAREN => b'(',
                    REPLACE_WITH_CLOSE_PAREN => b')',
                    _ => b';',
                };
                at += 1;
            }
            // in-place ranges are ASCII (the eligibility check), so CR/LF
            // are the only line breaks that can appear here
            for b in &mut buffer[at..end as usize] {
                if *b != b'\n' && *b != b'\r' {
                    *b = b' ';
                }
            }
        }
        // only ASCII bytes were written over valid UTF-8; validity survives
        String::from_utf8(buffer).expect("in-place writes are ASCII-only")
    }
}

/// Preserve newlines inside [start, end); everything else becomes one space
/// per UTF-16 code unit (a non-BMP char becomes 2). Ranges pushed out of
/// source order write nothing. `keep_line_breaks` additionally preserves
/// U+2028/U+2029, whose 3 UTF-8 bytes replace their own width.
fn write_space(out: &mut Vec<u8>, input: &str, start: u32, end: u32, keep_line_breaks: bool) {
    if start >= end {
        return;
    }
    let bytes = &input.as_bytes()[start as usize..end as usize];
    if bytes.is_ascii() {
        for &b in bytes {
            out.push(if b == b'\n' || b == b'\r' { b } else { b' ' });
        }
        return;
    }
    for c in input[start as usize..end as usize].chars() {
        match c {
            '\n' => out.push(b'\n'),
            '\r' => out.push(b'\r'),
            '\u{2028}' | '\u{2029}' if keep_line_breaks => {
                let mut encoded = [0u8; 3];
                out.extend_from_slice(c.encode_utf8(&mut encoded).as_bytes());
            }
            _ => {
                for _ in 0..c.len_utf16() {
                    out.push(b' ');
                }
            }
        }
    }
}

impl BlankString {
    /// UTF-16 variant of [`build`](Self::build) for the lossless path:
    /// `byte_to_unit` maps parse-copy byte offsets to unit indices (strictly
    /// increasing). Blanked ranges keep newline units; every other unit — lone
    /// surrogates included — becomes a single space.
    pub fn build_units(&self, units: &[u16], byte_to_unit: &[u32]) -> Vec<u16> {
        if self.ranges.is_empty() {
            return units.to_vec();
        }
        let unit_at = |pos: u32| byte_to_unit.partition_point(|&b| b < pos);

        let mut extra = 0usize;
        for &(flags, start, end, text_index) in &self.ranges {
            if flags == REPLACE_WITH_TEXT {
                let len = match &self.texts[text_index as usize] {
                    SpliceText::Str(text) => text.chars().map(char::len_utf16).sum(),
                    SpliceText::Units(units) => units.len(),
                };
                let u0 = unit_at(start);
                let u1 = unit_at(end);
                extra += len.saturating_sub(u1 - u0);
            }
        }

        let mut out: Vec<u16> = Vec::with_capacity(units.len() + extra);
        let mut previous_end = 0u32;
        let mut previous_unit = 0usize;

        for &(raw_flags, start, end, text_index) in &self.ranges {
            let keep_line_breaks = raw_flags & KEEP_LINE_BREAKS != 0;
            let flags = raw_flags & !KEEP_LINE_BREAKS;
            let range_start = start.max(previous_end);
            let range_unit = unit_at(range_start);
            out.extend_from_slice(&units[previous_unit..range_unit]);

            let mut range_unit = range_unit;
            match flags {
                REPLACE_WITH_TEXT => match &self.texts[text_index as usize] {
                    SpliceText::Str(text) => out.extend(text.encode_utf16()),
                    SpliceText::Units(units) => out.extend_from_slice(units),
                },
                REPLACE_WITH_CLOSE_PAREN => {
                    out.push(0x29);
                    range_unit += 1;
                }
                REPLACE_WITH_SEMI => {
                    out.push(0x3B);
                    range_unit += 1;
                }
                REPLACE_WITH_OPEN_PAREN => {
                    out.push(0x28);
                    range_unit += 1;
                }
                _ => {}
            }

            previous_end = end;
            if flags != REPLACE_WITH_TEXT {
                let end_unit = unit_at(previous_end);
                for &unit in &units[range_unit..end_unit] {
                    out.push(match unit {
                        0x0A | 0x0D => unit,
                        0x2028 | 0x2029 if keep_line_breaks => unit,
                        _ => 0x20,
                    });
                }
            }
            previous_unit = unit_at(previous_end);
        }

        out.extend_from_slice(&units[previous_unit..]);
        out
    }

    /// [`build_units`](Self::build_units) consuming the input units: every
    /// unit maps 1:1 (no UTF-8 width to shrink), so with no text splices the
    /// units are mutated in place and handed back. Falls back to
    /// [`build_units`](Self::build_units) on text splices or overlapping
    /// ranges, mirroring [`build_owned`](Self::build_owned).
    pub fn build_units_owned(self, units: Vec<u16>, byte_to_unit: &[u32]) -> Vec<u16> {
        let mut covered_end = 0u32;
        let in_place = self.ranges.iter().all(|&(raw_flags, start, end, _)| {
            let flags = raw_flags & !KEEP_LINE_BREAKS;
            let ok = flags != REPLACE_WITH_TEXT
                && start >= covered_end
                && (flags == REPLACE_WITH_BLANK || end > start);
            covered_end = covered_end.max(end);
            ok
        });
        if !in_place {
            return self.build_units(&units, byte_to_unit);
        }

        let unit_at = |pos: u32| byte_to_unit.partition_point(|&b| b < pos);
        let mut buffer = units;
        for &(raw_flags, start, end, _) in &self.ranges {
            let keep_line_breaks = raw_flags & KEEP_LINE_BREAKS != 0;
            let flags = raw_flags & !KEEP_LINE_BREAKS;
            let mut at = unit_at(start);
            if flags != REPLACE_WITH_BLANK {
                buffer[at] = match flags {
                    REPLACE_WITH_OPEN_PAREN => 0x28,
                    REPLACE_WITH_CLOSE_PAREN => 0x29,
                    _ => 0x3B,
                };
                at += 1;
            }
            for unit in &mut buffer[at..unit_at(end)] {
                let line_break = matches!(*unit, 0x0A | 0x0D)
                    || (keep_line_breaks && matches!(*unit, 0x2028 | 0x2029));
                if !line_break {
                    *unit = 0x20;
                }
            }
        }
        buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_preserves_newlines_and_length() {
        let input = "const a: number = 1";
        let mut bs = BlankString::default();
        // Blank the `: number` annotation (8 chars, no newlines).
        bs.blank(7, 15);
        assert_eq!(bs.build(input), format!("const a{} = 1", " ".repeat(8)));
    }

    #[test]
    fn blank_preserves_crlf_and_replaces_ls_ps_with_spaces() {
        // Pinned behavior: blanked regions keep CR/LF as line breaks; U+2028
        // and U+2029 become single spaces, matching the reference. One space
        // per UTF-16 unit keeps the length stable either way.
        let input = "a\nb\r\nc\u{2028}d\u{2029}e";
        let mut bs = BlankString::default();
        bs.blank(0, input.len() as u32);
        assert_eq!(bs.build(input), " \n \r\n     ");
    }

    #[test]
    fn build_units_preserves_crlf_and_replaces_ls_ps_with_spaces() {
        // The UTF-16 path pins the same behavior. byte_to_unit mirrors
        // transpile_units' construction (parse-copy byte offset after each
        // unit, plus the total sentinel).
        let units: Vec<u16> = "a\u{2028}b\u{2029}c".encode_utf16().collect();
        let byte_to_unit = [0u32, 1, 4, 5, 8, 9];
        let mut bs = BlankString::default();
        bs.blank(0, 9);
        assert_eq!(bs.build_units(&units, &byte_to_unit), vec![0x20; 5]);
    }

    #[test]
    fn no_ranges_returns_input() {
        let bs = BlankString::default();
        assert_eq!(bs.build("abc"), "abc");
    }

    #[test]
    fn build_owned_matches_build_on_the_fast_path() {
        let input = "const a: number = 1;\ninterface S { m(): void }";
        let make = || {
            let mut bs = BlankString::default();
            bs.blank(7, 15);
            bs.blank_but_start_with_semi(18, 46);
            bs
        };
        assert_eq!(make().build(input), make().build_owned(input.to_string()));
    }

    #[test]
    fn build_owned_falls_back_on_text_overwrites() {
        let input = "enum E { A }";
        let make = || {
            let mut bs = BlankString::default();
            bs.override_range(
                0,
                11,
                "var E;(function (E) { E[E[\"A\"] = 0] = \"A\"; })(E || (E = {}));",
            );
            bs
        };
        assert_eq!(make().build(input), make().build_owned(input.to_string()));
        assert!(make().build(input).len() > input.len());
    }

    #[test]
    fn build_owned_falls_back_on_non_ascii_blanks() {
        // U+00E9 is 2 UTF-8 bytes but 1 UTF-16 unit: blanking shrinks the
        // output, which cannot happen in place.
        let input = "a: é = 1";
        let mut bs = BlankString::default();
        bs.blank(2, 5);
        let slow = bs.build(input);
        assert_eq!(slow.len(), input.len() - 1);
        assert_eq!(bs.build_owned(input.to_string()), slow);
    }

    #[test]
    fn build_owned_falls_back_on_overlapping_ranges() {
        let input = "let x: T = 1;";
        let mut bs = BlankString::default();
        bs.blank(4, 9);
        bs.blank(6, 12); // overlaps the first range
        let slow = bs.build(input);
        assert_eq!(bs.build_owned(input.to_string()), slow);
    }

    #[test]
    fn build_units_owned_matches_build_units() {
        let units: Vec<u16> = "a\nb: c = 1".encode_utf16().collect();
        let byte_to_unit: Vec<u32> = (0..=units.len() as u32).collect();
        let mut bs = BlankString::default();
        bs.blank(3, 7);
        let slow = bs.build_units(&units, &byte_to_unit);
        assert_eq!(bs.build_units_owned(units, &byte_to_unit), slow);
    }
}
