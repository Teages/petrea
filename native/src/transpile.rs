use std::sync::OnceLock;

use oxc_allocator::{Allocator, AllocatorPool};
use oxc_diagnostics::NamedSource;
use oxc_parser::Parser;
use oxc_parser::ParserReturn;
use oxc_parser::config::TokensParserConfig;
use oxc_span::SourceType;

/// Shared arena pool. The async entry runs on the libuv thread pool, so
/// concurrent transpiles must not share one arena; reuse keeps the parser
/// from re-faulting fresh memory on every call.
pub(crate) fn allocator_pool() -> &'static AllocatorPool {
    static POOL: OnceLock<AllocatorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        AllocatorPool::new(threads)
    })
}

use crate::blank::blanker::UnsupportedSyntax;
use crate::replace::{ReplaceOptions, ReplaceParams, Replaces};
use crate::visit::walk::{blank_program, blank_program_utf16};

/// Build (and validate) the replace table; an empty map disables the rewrite.
fn build_replaces(replace: Option<ReplaceParams>) -> Result<Option<Replaces>, String> {
    replace
        .map(|params| {
            Replaces::new(
                &params.entries,
                ReplaceOptions {
                    prevent_assignment: params.prevent_assignment,
                    object_guards: params.object_guards,
                },
            )
        })
        .transpose()
        .map_err(|error| format!("invalid replace: {error}"))
        .map(|replaces| replaces.filter(|replaces| !replaces.is_empty()))
}

/// Render the parse's error diagnostics against `seen`, the text the failing
/// parse actually looked at.
fn render_errors(parse: &ParserReturn<'_>, filename: &str, seen: &str) -> String {
    parse
        .diagnostics
        .iter()
        .map(|d| {
            d.clone()
                .render_with_source_code(NamedSource::new(filename, seen.to_string()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The ordinary parse's gate: an unusable parse (nothing parsed, hard
/// errors) becomes a syntax error, while soft errors still produce a
/// recovered AST, which is processed like any other.
fn reject_unusable(
    parse: &ParserReturn<'_>,
    filename: &str,
    seen: &str,
    context: &str,
) -> Result<(), String> {
    if parse.program.body.is_empty()
        && parse.program.directives.is_empty()
        && parse.diagnostics.has_errors()
    {
        return Err(format!(
            "failed to parse {filename}{context}:\n{}",
            render_errors(parse, filename, seen)
        ));
    }
    Ok(())
}

/// The post-replace parse's gate, deliberately stricter than
/// [`reject_unusable`]: *any* error diagnostic — including a recoverable
/// one, where oxc hands back a runnable-looking program for top-level
/// `return;` and friends — rejects, so a replacement that manufactured an
/// invalid or recovered program fails loudly.
fn reject_replaced(parse: &ParserReturn<'_>, filename: &str, seen: &str) -> Result<(), String> {
    if parse.diagnostics.has_errors() {
        return Err(format!(
            "failed to parse {filename} (after replace):\n{}",
            render_errors(parse, filename, seen)
        ));
    }
    Ok(())
}

pub struct TranspileOutput {
    pub code: String,
    pub unsupported: Vec<UnsupportedSyntax>,
}

/// UTF-16 path output: code units, with report offsets already UTF-16 code units.
pub struct TranspileUnitsOutput {
    pub code: Vec<u16>,
    pub unsupported: Vec<UnsupportedSyntax>,
}

/// The API contract (`types.ts`) promises JS string indices (UTF-16 code
/// units); internal spans are UTF-8 bytes, so convert.
fn utf16_offset(input: &str, byte_offset: u32) -> u32 {
    if input.is_ascii() {
        return byte_offset;
    }
    let mut offset = byte_offset as usize;
    while offset > 0 && !input.is_char_boundary(offset) {
        offset -= 1;
    }
    input[..offset].chars().map(|c| c.len_utf16() as u32).sum()
}

/// Replace TypeScript-only syntax with whitespace, keeping the remaining
/// JavaScript byte-for-byte at its original line and column positions.
///
/// With `replace` matching at least once, the rewritten text is what the
/// pipeline parses (under the total [`reject_replaced`] gate) and what
/// every downstream pass consumes; positions are relative to the rewritten
/// source. With no matches the transpile is byte-for-byte like one without
/// `replace`, and the rewrite needs no parse — an unparseable original
/// still rewrites, so placeholder flows work.
///
/// Runtime TypeScript (enums, namespaces with runtime code, parameter
/// properties, `export =`, `import x = require(...)`, `<T>expr`) is kept
/// verbatim and reported through `TranspileOutput::unsupported`. Inputs that
/// are not valid TypeScript — including `as`/`satisfies` erasures that would
/// change operator grouping, which TypeScript itself rejects — return an error.
///
/// Takes the input by value: the napi boundary already hands over an owned
/// `String`, and the output side can reuse that buffer in place when the
/// edits allow it (see `blank_string::BlankString::build_owned`).
pub fn transpile(
    input: String,
    filename: &str,
    replace: Option<ReplaceParams>,
) -> Result<TranspileOutput, String> {
    let allocator_guard = allocator_pool().get();
    let allocator: &Allocator = &allocator_guard;
    let replaces = build_replaces(replace)?;
    // unknown/no extension falls back to plain JavaScript (module, no JSX):
    // TypeScript syntax fails there instead of parsing as TS
    let source_type = SourceType::from_path(filename)
        .unwrap_or_else(|_| SourceType::mjs())
        .with_module(true);

    // the scan consults no parse: it decides which text the pipeline parses
    let rewritten: Option<String> = match replaces.as_ref() {
        Some(replaces) => {
            let (rewritten, hits) = replaces.rewrite(input.as_str());
            (hits > 0).then_some(rewritten)
        }
        None => None,
    };
    let parse = match &rewritten {
        // hits: the total gate rejects any error, recoverable ones included
        Some(rewritten) => {
            let reparsed = Parser::new(allocator, rewritten.as_str(), source_type)
                .with_config(TokensParserConfig)
                .parse();
            reject_replaced(&reparsed, filename, rewritten)?;
            reparsed
        }
        // no replace, or nothing matched: the ordinary wide gate
        None => {
            let parsed = Parser::new(allocator, input.as_str(), source_type)
                .with_config(TokensParserConfig)
                .parse();
            reject_unusable(&parsed, filename, &input, "")?;
            parsed
        }
    };

    let source: &str = rewritten.as_deref().unwrap_or(input.as_str());
    let (output, unsupported) = blank_program(&parse.program, source, &parse.tokens);
    // the report offsets are converted while the source the edits were built
    // against is still alive (the replaced text when replace ran)
    let unsupported = unsupported
        .into_iter()
        .map(|report| UnsupportedSyntax {
            start: utf16_offset(source, report.start),
            end: utf16_offset(source, report.end),
            ..report
        })
        .collect();

    let code = if let Some(rewritten) = rewritten {
        output.build_owned(rewritten)
    } else {
        output.build_owned(input)
    };
    Ok(TranspileOutput { code, unsupported })
}

/// The lossy UTF-8 parse copy of raw code units: surrogate pairs become the
/// astral character, every other unit maps to itself when possible, U+FFFD
/// otherwise; the byte length per unit is tracked so spans can map back.
pub(crate) fn lossy_copy(units: &[u16]) -> (String, Vec<u32>) {
    let mut copy: Vec<u8> = Vec::with_capacity(units.len() * 3);
    let mut byte_to_unit: Vec<u32> = Vec::with_capacity(units.len() + 1);
    let mut index = 0usize;
    while index < units.len() {
        byte_to_unit.push(copy.len() as u32);
        let unit = units[index];
        let is_high = (0xD800..0xDC00).contains(&unit);
        let next_low =
            matches!(units.get(index + 1), Some(next) if (0xDC00..0xE000).contains(next));
        let mut buf = [0u8; 4];
        if is_high && next_low {
            let code =
                0x10000 + (((unit - 0xD800) as u32) << 10) + (units[index + 1] - 0xDC00) as u32;
            copy.extend_from_slice(
                char::from_u32(code)
                    .expect("valid pair")
                    .encode_utf8(&mut buf)
                    .as_bytes(),
            );
            // the low surrogate needs its own map entry or every later unit
            // index shifts by one; the entry points at the middle of the
            // astral char's bytes — no span boundary can land there
            byte_to_unit.push((copy.len() - 2) as u32);
            index += 2;
        } else if unit == 0x0A || unit == 0x0D {
            copy.push(unit as u8);
            index += 1;
        } else {
            let c = char::from_u32(unit as u32).unwrap_or('\u{FFFD}');
            copy.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            index += 1;
        }
    }
    byte_to_unit.push(copy.len() as u32);
    let parse_copy = String::from_utf8(copy).expect("lossy copy is valid UTF-8");
    (parse_copy, byte_to_unit)
}

/// Lossless UTF-16 variant of [`transpile`]: the parser works on a lossy UTF-8
/// copy (raw lone surrogates cannot exist in Rust strings), but every output
/// unit is taken from the original units, so raw lone surrogates survive.
/// Report offsets are UTF-16 code units. Like [`transpile`], the units are
/// taken by value so the output can reuse the buffer in place.
///
/// The replace rewrite scans and splices the original units directly — the
/// lossy copy never participates in matching.
pub fn transpile_units(
    units: Vec<u16>,
    filename: &str,
    replace: Option<ReplaceParams>,
) -> Result<TranspileUnitsOutput, String> {
    let allocator_guard = allocator_pool().get();
    let allocator: &Allocator = &allocator_guard;
    let replaces = build_replaces(replace)?;
    // same extension fallback as [`transpile`]
    let source_type = SourceType::from_path(filename)
        .unwrap_or_else(|_| SourceType::mjs())
        .with_module(true);

    // the rewrite runs on the ORIGINAL units: a key spelled U+FFFD cannot
    // match a lone surrogate the lossy copy merely represents
    let rewritten: Option<Vec<u16>> = match replaces.as_ref() {
        Some(replaces) => {
            let (rewritten, hits) = replaces.rewrite_units(&units);
            (hits > 0).then_some(rewritten)
        }
        None => None,
    };
    // whichever text the pipeline parses gets its own lossy copy and map;
    // the parse borrows the copy, so the bindings live here at function level
    let (units_source, parse_copy, byte_to_unit) = match &rewritten {
        Some(rewritten) => {
            let (copy, map) = lossy_copy(rewritten);
            (rewritten.as_slice(), copy, map)
        }
        None => {
            let (copy, map) = lossy_copy(&units);
            (units.as_slice(), copy, map)
        }
    };
    // hits: the total gate; no replace or no hits: the ordinary wide gate
    let parse = Parser::new(allocator, parse_copy.as_str(), source_type)
        .with_config(TokensParserConfig)
        .parse();
    if rewritten.is_some() {
        reject_replaced(&parse, filename, &parse_copy)?;
    } else {
        reject_unusable(&parse, filename, &parse_copy, "")?;
    }

    let (output, unsupported) = blank_program_utf16(
        &parse.program,
        units_source,
        parse_copy.as_str(),
        &byte_to_unit,
        &parse.tokens[..],
    );

    let unit_at = |pos: u32| byte_to_unit.partition_point(|&b| b < pos) as u32;
    let unsupported = unsupported
        .into_iter()
        .map(|report| UnsupportedSyntax {
            start: unit_at(report.start),
            end: unit_at(report.end),
            ..report
        })
        .collect();

    let code = if let Some(rewritten) = rewritten {
        output.build_units_owned(rewritten, &byte_to_unit)
    } else {
        output.build_units_owned(units, &byte_to_unit)
    };
    Ok(TranspileUnitsOutput { code, unsupported })
}

/// [`transpile_units`] with panic containment (see [`transpile_caught`]).
pub fn transpile_units_caught(
    units: Vec<u16>,
    filename: &str,
    replace: Option<ReplaceParams>,
) -> Result<TranspileUnitsOutput, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transpile_units(units, filename, replace)
    })) {
        Ok(result) => result,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&'static str>()
                        .map(|s| (*s).to_string())
                })
                .unwrap_or_else(|| "panic".to_string());
            Err(format!("internal error: {detail}"))
        }
    }
}

/// [`transpile`] with panic containment: a bug in an untested AST corner must
/// surface as a JS exception, not abort the process — napi does not catch
/// unwinds by default, for the sync binding or async task compute alike.
pub fn transpile_caught(
    input: String,
    filename: &str,
    replace: Option<ReplaceParams>,
) -> Result<TranspileOutput, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transpile(input, filename, replace)
    })) {
        Ok(result) => result,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&'static str>()
                        .map(|s| (*s).to_string())
                })
                .unwrap_or_else(|| "panic".to_string());
            Err(format!("internal error: {detail}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn blanks_marker_between_comments() {
        // the `?`/`!` marker sits between two comments; locating it hops the trailing comment
        let output = transpile(
            "class C { private f2/**/!/**/: string; }".to_string(),
            "input.ts",
            None,
        )
        .unwrap();
        assert_eq!(output.code, "class C {         f2/**/ /**/        ; }");
    }

    #[test]
    fn reports_and_rejects() {
        let output = transpile(
            "class C { constructor(private a: string) {} }".to_string(),
            "input.ts",
            None,
        )
        .unwrap();
        assert_eq!(output.unsupported.len(), 1);
        assert_eq!(output.unsupported[0].node_type, "TSParameterProperty");

        assert!(transpile("1 + 1 as T / 2;".to_string(), "input.ts", None).is_err());
    }

    #[test]
    fn output_reuses_input_length_on_the_fast_path() {
        // a file with no enum expansions or grouping-constant text splices
        // blanks in place: output length equals input length, positions intact
        let input = "const a: number = 1;\nlet b = a as string;\ntype T = typeof a;\n";
        let output = transpile(input.to_string(), "input.ts", None).unwrap();
        assert_eq!(output.code.len(), input.len());
        assert_eq!(output.code.lines().count(), input.lines().count());
    }

    fn replace_params(entries: &[(&str, &str)]) -> Option<ReplaceParams> {
        Some(ReplaceParams {
            entries: entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            prevent_assignment: false,
            object_guards: false,
        })
    }

    #[test]
    fn replace_splices_and_rejects_empty_keys() {
        let output = transpile(
            "const mode = MODE;".to_string(),
            "input.ts",
            replace_params(&[("MODE", "x")]),
        )
        .unwrap();
        assert_eq!(output.code, "const mode = x;");

        let Err(error) = transpile("1".to_string(), "input.ts", replace_params(&[("", "1")]))
        else {
            panic!("empty replace keys must be rejected");
        };
        assert!(
            error.contains("invalid replace key \"\""),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn replace_with_zero_hits_matches_the_replace_free_transpile_byte_for_byte() {
        let input = "const a: number = KEY;\nenum E { A = FLAG }\nlet b = a as KEY;";
        let armed =
            transpile(input.to_string(), "input.ts", replace_params(&[("X", "1")])).unwrap();
        let plain = transpile(input.to_string(), "input.ts", None).unwrap();
        assert_eq!(armed.code, plain.code);
        assert_eq!(armed.unsupported, plain.unsupported);
    }

    #[test]
    fn replace_scans_without_a_parse_so_placeholder_flows_work() {
        // `const x = @VALUE@;` is not parseable TypeScript, yet the rewrite
        // needs no parse and runs first
        let output = transpile(
            "const x = @VALUE@;".to_string(),
            "input.ts",
            replace_params(&[("@VALUE@", "'1'")]),
        )
        .unwrap();
        assert_eq!(output.code, "const x = '1';");

        // zero hits fall to the ordinary gate: the failure names the original
        // parse, never replace
        let Err(error) = transpile(
            "const x = @VALUE@;".to_string(),
            "input.ts",
            replace_params(&[("ABSENT", "1")]),
        ) else {
            panic!("the unparseable original with no hits must fail");
        };
        assert!(
            error.contains("failed to parse input.ts:\n") && !error.contains("after replace"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_replacement_making_a_recovered_program_fails_the_strict_gate() {
        // oxc recovers a runnable-looking program for a top-level `return`
        // in a module; the post-replace gate is total, so it fails loudly
        let Err(error) = transpile(
            "FLAG;".to_string(),
            "input.ts",
            replace_params(&[("FLAG", "return")]),
        ) else {
            panic!("a replaced top-level return must fail loudly");
        };
        assert!(
            error.contains("failed to parse input.ts (after replace)"),
            "unexpected error: {error}"
        );

        // the same recovered shape the user wrote themselves still
        // transpiles under the ordinary wide gate
        let input = "return;\nlog(1);";
        let plain = transpile(input.to_string(), "input.ts", None).unwrap();
        let armed = transpile(
            input.to_string(),
            "input.ts",
            replace_params(&[("ABSENT", "1")]),
        )
        .unwrap();
        assert_eq!(plain.code, armed.code);
        assert_eq!(plain.code, input);
    }

    #[test]
    fn replace_runs_before_the_erasure_which_blanks_its_output() {
        let output = transpile(
            "const a: T = KEY;".to_string(),
            "input.ts",
            replace_params(&[("KEY", "1")]),
        )
        .unwrap();
        assert_eq!(output.code, "const a    = 1;");

        let output = transpile(
            "type T = KEY;".to_string(),
            "input.ts",
            replace_params(&[("KEY", "1")]),
        )
        .unwrap();
        assert_eq!(output.code, "           ");
    }

    #[test]
    fn replace_crossing_a_syntax_boundary_fails_the_reparse_loudly() {
        // a same-shape value stays valid TypeScript; a value that breaks the
        // statement is a loud parse failure of the rewritten text, not silent
        // garbage
        let output = transpile(
            "let x: T = 1;".to_string(),
            "input.ts",
            replace_params(&[("T =", "U =")]),
        )
        .unwrap();
        assert_eq!(output.code, "let x    = 1;");

        let Err(error) = transpile(
            "let x: T = 1;".to_string(),
            "input.ts",
            replace_params(&[("T =", ")")]),
        ) else {
            panic!("invalid replaced syntax must fail loudly");
        };
        assert!(
            error.contains("failed to parse input.ts (after replace)"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn replace_units_scans_the_original_units_so_ufffd_cannot_match_a_surrogate() {
        // a raw lone surrogate never answers a U+FFFD key: they differ at the
        // unit level, and the surrogate round-trips untouched
        let mut units: Vec<u16> = "log(\"a\")".encode_utf16().collect();
        units[5] = 0xD800;
        let output = transpile_units(
            units.clone(),
            "input.js",
            replace_params(&[("\u{FFFD}", "X")]),
        )
        .unwrap();
        assert_eq!(output.code, units);

        // a genuine U+FFFD in the same file still replaces
        let mut units: Vec<u16> = "log(\"a b\")".encode_utf16().collect();
        units[5] = 0xD800; // raw lone surrogate where 'a' stood
        units[7] = 0xFFFD; // a genuine replacement char where 'b' stood
        let output = transpile_units(
            units.clone(),
            "input.js",
            replace_params(&[("\u{FFFD}", "X")]),
        )
        .unwrap();
        units[7] = b'X' as u16;
        assert_eq!(output.code, units);
    }

    #[test]
    fn replace_applies_inside_kept_verbatim_unsupported_constructs() {
        let source = "namespace N { export const x = KEY; }".to_string();
        let output =
            transpile(source.clone(), "input.ts", replace_params(&[("KEY", "1")])).unwrap();
        assert_eq!(output.code, "namespace N { export const x = 1; }");
        assert_eq!(output.unsupported.len(), 1);
    }

    #[test]
    fn replace_splices_literal_and_comment_hits_exactly() {
        let output = transpile(
            "const s = \"FLAG\";".to_string(),
            "input.ts",
            replace_params(&[("FLAG", "1")]),
        )
        .unwrap();
        assert_eq!(output.code, "const s = \"1\";");

        let output = transpile(
            "const r = /FLAG/;".to_string(),
            "input.ts",
            replace_params(&[("FLAG", "1")]),
        )
        .unwrap();
        assert_eq!(output.code, "const r = /1/;");

        let output = transpile(
            "const t = `FLAG`;".to_string(),
            "input.ts",
            replace_params(&[("FLAG", "1")]),
        )
        .unwrap();
        assert_eq!(output.code, "const t = `1`;");

        let output = transpile(
            "const t = `${FLAG}`;".to_string(),
            "input.ts",
            replace_params(&[("FLAG", "1")]),
        )
        .unwrap();
        assert_eq!(output.code, "const t = `${1}`;");

        let output = transpile(
            "/* FLAG */".to_string(),
            "input.ts",
            replace_params(&[("FLAG", "1")]),
        )
        .unwrap();
        assert_eq!(output.code, "/* 1 */");
    }

    #[test]
    fn replace_renames_an_enum_self_consistently() {
        let output = transpile(
            "enum E { A }\nlog(E);".to_string(),
            "input.ts",
            replace_params(&[("E", "F")]),
        )
        .unwrap();
        assert_eq!(
            output.code,
            "var  F; (function (F) { F[F[\"A\"] = 0] = \"A\" })(F || (F = {}));\nlog(F);"
        );

        let output = transpile(
            "enum E { A }\nlog(E.A);".to_string(),
            "input.ts",
            replace_params(&[("A", "B")]),
        )
        .unwrap();
        assert_eq!(
            output.code,
            "var  E; (function (E) { E[E[\"B\"] = 0] = \"B\" })(E || (E = {}));\nlog(E.B);"
        );
    }

    #[test]
    fn replace_can_make_an_enum_invalid_and_fails_loudly() {
        let Err(error) = transpile(
            "enum E { A }\nif (E) { yes(); }".to_string(),
            "input.ts",
            replace_params(&[("E", "0")]),
        ) else {
            panic!("`enum 0` must fail the reparse loudly");
        };
        assert!(
            error.contains("failed to parse input.ts (after replace)"),
            "unexpected error: {error}"
        );

        // `if (F)` does not fold: an identifier is no literal
        let output = transpile(
            "enum E { A }\nif (E) { yes(); }".to_string(),
            "input.ts",
            replace_params(&[("E", "F")]),
        )
        .unwrap();
        assert_eq!(
            output.code,
            "var  F; (function (F) { F[F[\"A\"] = 0] = \"A\" })(F || (F = {}));\nif (F) { yes(); }"
        );
    }

    #[test]
    fn replace_feeds_the_enum_pipeline_consistently() {
        let output = transpile(
            "const n = 1;\nenum E { A = n }".to_string(),
            "input.ts",
            replace_params(&[("1", "2")]),
        )
        .unwrap();
        assert_eq!(
            output.code,
            "const n = 2;\nvar  E; (function (E) { E[E[\"A\"] = 2] = \"A\" })(E || (E = {}));"
        );

        let output = transpile(
            "enum E { A = 1, B = A + f() }".to_string(),
            "input.ts",
            replace_params(&[("A", "X")]),
        )
        .unwrap();
        assert_eq!(
            output.code,
            "var  E; (function (E) { E[E[\"X\"] = 1] = \"X\"; E[E[\"B\"] = E.X + f()] = \"B\" })(E || (E = {}));"
        );

        let output = transpile(
            "enum E { A = \"KEY\" }".to_string(),
            "input.ts",
            replace_params(&[("KEY", "1")]),
        )
        .unwrap();
        assert_eq!(
            output.code,
            "var  E; (function (E) { E[\"A\"] = \"1\" })(E || (E = {}));"
        );
    }
}
