//! Native Rust implementation of `petrea`: napi bindings whose parse+blank
//! work runs on a background thread (napi `AsyncTask`). The synchronous and
//! UTF-16 variants exist for the calling-thread and lossless-surrogate paths.

/// Flat index of a concrete AST node, assigned by the flatten pass in
/// `visit::walk::blank_program` (kept above the `mod` items: macros must precede use).
macro_rules! node_index {
    ($n:expr) => {
        $n.node_id.get().index() as u32
    };
}

mod blank;
mod transpile;
mod visit;

use napi::bindgen_prelude::{AsyncTask, Uint16Array};
use napi::{Env, Error, Result, Status, Task};
use napi_derive::napi;

/// Options for the native bindings. `onError` reporting comes back as data so
/// the background task never calls back into JS; the JS wrapper invokes the
/// user callback.
#[napi(object)]
pub struct TranspileNativeOptions {
    /// Parse the input as `ts` (default) or `tsx`.
    pub lang: Option<String>,
    /// Source path quoted in parse-failure diagnostics; its extension also
    /// selects the parse mode (a `.tsx` filename enables JSX).
    pub filename: Option<String>,
}

/// A TypeScript-only construct with runtime semantics that was kept verbatim.
#[napi(object)]
pub struct NativeUnsupported {
    pub node_type: String,
    pub start: u32,
    pub end: u32,
}

#[napi(object)]
pub struct TranspileNativeResult {
    pub code: String,
    pub unsupported: Vec<NativeUnsupported>,
}

/// UTF-16 variant of [`TranspileNativeResult`]: raw code units are the only
/// way to round-trip raw lone surrogates (a Rust `String` cannot hold them).
#[napi(object)]
pub struct TranspileUnitsResult {
    pub code: Uint16Array,
    pub unsupported: Vec<NativeUnsupported>,
}

fn resolve_filename(options: Option<&TranspileNativeOptions>) -> String {
    let lang = options.and_then(|o| o.lang.as_deref());
    options.and_then(|o| o.filename.clone()).unwrap_or_else(|| {
        if lang == Some("tsx") {
            "input.tsx".to_string()
        } else {
            "input.ts".to_string()
        }
    })
}

/// The API contract (`types.ts`) promises JS string indices (UTF-16 code
/// units); internal spans are UTF-8 bytes, so `transpile` converts the report
/// offsets itself while the input is still alive.
fn to_napi_units_result(
    output: std::result::Result<transpile::TranspileUnitsOutput, String>,
) -> Result<TranspileUnitsResult> {
    let output = output.map_err(|message| Error::new(Status::GenericFailure, message))?;
    Ok(TranspileUnitsResult {
        code: Uint16Array::new(output.code),
        unsupported: output
            .unsupported
            .into_iter()
            .map(|report| NativeUnsupported {
                node_type: report.node_type.to_string(),
                start: report.start,
                end: report.end,
            })
            .collect(),
    })
}

fn to_napi_result(
    output: std::result::Result<transpile::TranspileOutput, String>,
) -> Result<TranspileNativeResult> {
    let output = output.map_err(|message| Error::new(Status::GenericFailure, message))?;
    Ok(TranspileNativeResult {
        code: output.code,
        unsupported: output
            .unsupported
            .into_iter()
            .map(|report| NativeUnsupported {
                node_type: report.node_type.to_string(),
                start: report.start,
                end: report.end,
            })
            .collect(),
    })
}

pub struct TranspileTask {
    input: String,
    filename: String,
}

impl Task for TranspileTask {
    type Output = TranspileNativeResult;
    type JsValue = TranspileNativeResult;

    fn compute(&mut self) -> Result<Self::Output> {
        to_napi_result(transpile::transpile_caught(
            std::mem::take(&mut self.input),
            &self.filename,
        ))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// Async entry point: parses and blanks on a background thread; rejects with
/// a `SyntaxError`-style error when the input cannot be parsed.
#[napi(ts_return_type = "Promise<TranspileNativeResult>")]
pub fn transpile_async(
    input: String,
    options: Option<TranspileNativeOptions>,
) -> AsyncTask<TranspileTask> {
    let filename = resolve_filename(options.as_ref());
    AsyncTask::new(TranspileTask { input, filename })
}

/// Synchronous counterpart of `transpile_async` (the JS `transpileSync` export).
#[napi]
pub fn transpile_native_sync(
    input: String,
    options: Option<TranspileNativeOptions>,
) -> Result<TranspileNativeResult> {
    let filename = resolve_filename(options.as_ref());
    to_napi_result(transpile::transpile_caught(input, &filename))
}

#[cfg(test)]
mod perf_bench {
    //! Internal performance harness:
    //! `cargo test --release perf_bench -- --ignored --nocapture`
    //! (`--release` required; debug numbers are meaningless).

    use std::fs;
    use std::time::Instant;

    use oxc_allocator::{Allocator, AllocatorPool};
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    use crate::transpile::allocator_pool;

    fn corpus() -> String {
        corpus_filtered(|_| true)
    }

    /// Same corpus without the enum-declaring files: no text splices occur,
    /// so the output side takes the in-place rewrite path.
    fn corpus_without_enums() -> String {
        corpus_filtered(|content| !content.contains("enum "))
    }

    fn corpus_filtered(keep: impl Fn(&str) -> bool) -> String {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../test/fixture");
        let mut out = String::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "ts") {
                let content = fs::read_to_string(path).unwrap();
                if keep(&content) {
                    out.push_str(&content);
                    out.push('\n');
                }
            }
        }
        out
    }

    fn parse_only(pool: &AllocatorPool, input: &str, tokens: bool) {
        let guard = pool.get();
        let allocator: &Allocator = &guard;
        let source_type = SourceType::from_path("input.ts").unwrap().with_module(true);
        let parser = Parser::new(allocator, input, source_type);
        let ret = if tokens {
            parser
                .with_config(oxc_parser::config::TokensParserConfig)
                .parse()
        } else {
            parser.parse()
        };
        std::hint::black_box((ret.program.body.len(), ret.tokens.len()));
    }

    fn time_it<F: FnMut()>(iters: usize, mut f: F) -> (u128, u128) {
        for _ in 0..10 {
            f();
        }
        let mut total = 0u128;
        let mut min = u128::MAX;
        for _ in 0..iters {
            let start = Instant::now();
            f();
            let elapsed = start.elapsed().as_nanos();
            total += elapsed;
            min = min.min(elapsed);
        }
        (min, total / iters as u128)
    }

    #[test]
    #[ignore]
    fn floors_and_pipeline() {
        let original = corpus();
        let mut corpus = original.clone();
        while corpus.len() < 100_000 {
            corpus.push_str(&original);
        }
        let original_plain = corpus_without_enums();
        let mut corpus_plain = original_plain.clone();
        while corpus_plain.len() < 100_000 {
            corpus_plain.push_str(&original_plain);
        }
        println!(
            "input: {} bytes ({} without enums)",
            corpus.len(),
            corpus_plain.len()
        );

        transpile_for_bench(&corpus);
        transpile_for_bench(&corpus_plain);

        let iters = std::env::var("PERF_ITERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(100);
        let pool = allocator_pool();
        let (p_min, p_mean) = time_it(iters, || parse_only(pool, &corpus, false));
        let (pt_min, pt_mean) = time_it(iters, || parse_only(pool, &corpus, true));
        // the input copy stands in for the JS-string → Rust-String copy the
        // napi boundary always performs; timed separately so the pipeline
        // cost (flatten + walk + output) can be derived by subtraction
        let (c_min, c_mean) = time_it(iters, || {
            let copy = corpus.clone();
            std::hint::black_box(&copy);
        });
        let (t_min, t_mean) = time_it(iters, || {
            transpile_for_bench(&corpus);
        });
        let (tp_min, tp_mean) = time_it(iters, || {
            transpile_for_bench(&corpus_plain);
        });
        println!(
            "parse-only      min={:>5}us mean={:>5}us",
            p_min / 1000,
            p_mean / 1000
        );
        println!(
            "parse+tokens    min={:>5}us mean={:>5}us",
            pt_min / 1000,
            pt_mean / 1000
        );
        println!(
            "input copy      min={:>5}us mean={:>5}us",
            c_min / 1000,
            c_mean / 1000
        );
        println!(
            "full transpile  min={:>5}us mean={:>5}us  (enum corpus, output fallback path)",
            t_min / 1000,
            t_mean / 1000
        );
        println!(
            "no-enum variant min={:>5}us mean={:>5}us  (output rewrites in place)",
            tp_min / 1000,
            tp_mean / 1000
        );
    }

    fn transpile_for_bench(input: &str) -> usize {
        // the clone stands in for the JS-string → Rust-String copy the napi
        // boundary always performs, so the in-place output path is measured
        let output =
            crate::transpile::transpile(input.to_string(), "input.ts").expect("transpiles");
        std::hint::black_box(output.code.len() + output.unsupported.len())
    }
}

pub struct TranspileUnitsTask {
    units: Vec<u16>,
    filename: String,
}

impl Task for TranspileUnitsTask {
    type Output = TranspileUnitsResult;
    type JsValue = TranspileUnitsResult;

    fn compute(&mut self) -> Result<Self::Output> {
        to_napi_units_result(transpile::transpile_units_caught(
            std::mem::take(&mut self.units),
            &self.filename,
        ))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// UTF-16 entry point: raw lone surrogates — which cannot cross into a Rust
/// `String` — are handled losslessly here; report offsets are UTF-16 units.
#[napi(ts_return_type = "Promise<TranspileUnitsResult>")]
pub fn transpile_utf16_async(
    units: Uint16Array,
    options: Option<TranspileNativeOptions>,
) -> AsyncTask<TranspileUnitsTask> {
    let filename = resolve_filename(options.as_ref());
    let units = units.to_vec();
    AsyncTask::new(TranspileUnitsTask { units, filename })
}

/// Synchronous UTF-16 entry point (see [`transpile_utf16_async`]).
#[napi]
pub fn transpile_utf16_sync(
    units: Uint16Array,
    options: Option<TranspileNativeOptions>,
) -> Result<TranspileUnitsResult> {
    let filename = resolve_filename(options.as_ref());
    // one copy into an owned buffer — the output side reuses it in place
    to_napi_units_result(transpile::transpile_units_caught(units.to_vec(), &filename))
}
