//! JS string-literal text for enum members: escape decoding over UTF-16 code
//! units (lossless for lone surrogates) and the `JSON.stringify` quote forms
//! the TypeScript emitter uses for member keys and folded string values.

use oxc_ast::ast::StringLiteral;
use oxc_span::Span;

use crate::visit::walk::unit_at;

/// The source decoding context string folding reads from: the parse copy,
/// plus the original code units with their byte map on the UTF-16 path. A
/// standalone value (not a `Walker` borrow), so resolution sessions can hold
/// it without freezing the walker.
#[derive(Clone, Copy)]
pub(crate) struct SourceText<'a> {
    pub src: &'a str,
    pub units: Option<&'a [u16]>,
    pub byte_to_unit: Option<&'a [u32]>,
}

impl<'a> SourceText<'a> {
    /// The UTF-16 units of `span` — original units on the UTF-16 path
    /// (lossless for lone surrogates), parse copy characters otherwise.
    pub fn original_span_units(&self, span: Span) -> Vec<u16> {
        match (self.units, self.byte_to_unit) {
            (Some(units), Some(byte_to_unit)) => {
                let u0 = unit_at(byte_to_unit, span.start) as usize;
                let u1 = unit_at(byte_to_unit, span.end) as usize;
                units[u0..u1].to_vec()
            }
            _ => self.src[span.start as usize..span.end as usize]
                .chars()
                .flat_map(|c| {
                    let mut buf = [0u16; 2];
                    let encoded = c.encode_utf16(&mut buf);
                    encoded.to_vec()
                })
                .collect(),
        }
    }
}

/// Decoded UTF-16 value of a string literal, from the original code units on
/// the UTF-16 path — the lossy parse copy would corrupt raw lone surrogates.
pub(crate) fn string_literal_value_units(
    source: &SourceText<'_>,
    literal: &StringLiteral<'_>,
) -> Vec<u16> {
    let span = literal.span;
    let inner_start = span.start + 1;
    let inner_end = span.end - 1;
    match (source.units, source.byte_to_unit) {
        (Some(units), Some(byte_to_unit)) => {
            // decode from the original code units, which preserve raw
            // lone surrogates losslessly
            let u0 = unit_at(byte_to_unit, inner_start);
            let u1 = unit_at(byte_to_unit, inner_end);
            decode_units(&units[u0 as usize..u1 as usize])
        }
        _ => {
            let units: Vec<u16> = source.src[inner_start as usize..inner_end as usize]
                .chars()
                .flat_map(|c| {
                    let mut buf = [0u16; 2];
                    let encoded = c.encode_utf16(&mut buf);
                    encoded.to_vec()
                })
                .collect();
            decode_units(&units)
        }
    }
}

/// Decode JS string-literal escapes over UTF-16 code units; lone surrogates
/// (`\uD800`-style escapes or raw units) stay as individual units.
pub(super) fn decode_units(units: &[u16]) -> Vec<u16> {
    decode_units_inner(units, false)
}

/// [`decode_units`] for template quasis: literal CRLF and lone CR normalize
/// to LF, like every cooked value; an escaped `\r` stays CR.
pub(super) fn decode_template_units(units: &[u16]) -> Vec<u16> {
    decode_units_inner(units, true)
}

fn decode_units_inner(units: &[u16], template: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0usize;
    while i < units.len() {
        let unit = units[i];
        if unit != 0x5C {
            if template && unit == 0x0D {
                // literal <CR><LF> and lone <CR> normalize to <LF>
                if units.get(i + 1) == Some(&0x0A) {
                    i += 1;
                }
                out.push(0x0A);
            } else {
                out.push(unit);
            }
            i += 1;
            continue;
        }
        i += 1;
        let Some(&escape) = units.get(i) else {
            out.push(0x5C);
            break;
        };
        i += 1;
        match escape {
            0x0A | 0x0D | 0x2028 | 0x2029 => {
                // line continuation; a CR/LF pair is consumed together
                if escape == 0x0D && units.get(i) == Some(&0x0A) {
                    i += 1;
                }
            }
            0x62 => out.push(0x08),
            0x74 => out.push(0x09),
            0x6E => out.push(0x0A),
            0x76 => out.push(0x0B),
            0x66 => out.push(0x0C),
            0x72 => out.push(0x0D),
            0x78 => {
                let value = hex_value(units, i, 2);
                if let Some((value, consumed)) = value {
                    out.push(value as u16);
                    i += consumed;
                } else {
                    out.push(0x78);
                }
            }
            0x75 => {
                if units.get(i) == Some(&0x7B) {
                    // \u{HexDigits}
                    let mut j = i + 1;
                    let mut value: u32 = 0;
                    while let Some(&digit) = units.get(j) {
                        let d = hex_digit(digit);
                        match d {
                            Some(d) if value <= 0x10FFFF => value = value * 16 + d as u32,
                            _ => break,
                        }
                        j += 1;
                    }
                    if units.get(j) == Some(&0x7D) && value <= 0x10FFFF {
                        // lone surrogates stay as single units and get
                        // re-escaped by json_quote_utf16
                        if (0xD800..=0xDFFF).contains(&value) {
                            out.push(value as u16);
                        } else {
                            let mut buf = [0u16; 2];
                            let encoded = char::from_u32(value).unwrap().encode_utf16(&mut buf);
                            out.extend(encoded.iter().copied());
                        }
                        i = j + 1;
                    } else {
                        out.push(0x75);
                    }
                } else {
                    let value = hex_value(units, i, 4);
                    if let Some((value, consumed)) = value {
                        out.push(value as u16);
                        i += consumed;
                    } else {
                        out.push(0x75);
                    }
                }
            }
            0x30..=0x37 => {
                // Annex B legacy octal: digits starting 0-3 consume up to two
                // more octal digits, digits starting 4-7 up to one
                let first = escape - 0x30;
                let max_extra = if first <= 3 { 2 } else { 1 };
                let mut value = first;
                let mut count = 1usize;
                while count <= max_extra
                    && let Some(&digit) = units.get(i)
                    && (0x30..=0x37).contains(&digit)
                {
                    value = value * 8 + (digit - 0x30);
                    i += 1;
                    count += 1;
                }
                out.push(value);
            }
            other => out.push(other),
        }
    }
    out
}

fn hex_digit(unit: u16) -> Option<u16> {
    match unit {
        0x30..=0x39 => Some(unit - 0x30),
        0x41..=0x46 => Some(unit - 0x37),
        0x61..=0x66 => Some(unit - 0x57),
        _ => None,
    }
}

fn hex_value(units: &[u16], start: usize, count: usize) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    for offset in 0..count {
        let digit = hex_digit(*units.get(start + offset)?)?;
        value = value * 16 + digit as u32;
    }
    Some((value, count))
}

/// `JSON.stringify` over UTF-16 code units: well-formed output — lone
/// surrogates escape as lowercase `\udXXX`, pairs become the astral character.
pub(super) fn json_quote_utf16(units: &[u16]) -> String {
    let mut out = String::from("\"");
    let mut i = 0usize;
    while i < units.len() {
        let unit = units[i];
        match unit {
            0x22 => out.push_str("\\\""),
            0x5C => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0A => out.push_str("\\n"),
            0x0C => out.push_str("\\f"),
            0x0D => out.push_str("\\r"),
            0x00..=0x1F => out.push_str(&format!("\\u{:04x}", unit)),
            0xD800..=0xDBFF => {
                let next = units.get(i + 1).copied();
                if matches!(next, Some(low) if (0xDC00..=0xDFFF).contains(&low)) {
                    let code = 0x10000
                        + (((unit - 0xD800) as u32) << 10)
                        + (next.unwrap() - 0xDC00) as u32;
                    out.push(char::from_u32(code).expect("valid astral char"));
                    i += 1;
                } else {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            0xDC00..=0xDFFF => out.push_str(&format!("\\u{:04x}", unit)),
            _ => out.push(char::from_u32(unit as u32).expect("BMP char")),
        }
        i += 1;
    }
    out.push('"');
    out
}

/// `JSON.stringify` of a JS string, used for the quoted member key text.
pub(super) fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
