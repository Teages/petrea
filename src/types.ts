/**
 * Information about a TypeScript-only construct that has runtime semantics and
 * therefore cannot be erased. Positions are character offsets into the input.
 */
export interface UnsupportedSyntax {
  readonly type: string
  readonly start: number
  readonly end: number
}

export type OnError = (node: UnsupportedSyntax) => void

/**
 * A recoverable condition at a source site: a define substitution was
 * skipped because splicing there would change meaning (e.g. a literal or
 * lowercase value in a JSX tag position). Positions are character offsets
 * into the input.
 */
export interface Warning {
  readonly type: string
  readonly start: number
  readonly end: number
}

export type OnWarn = (warning: Warning) => void

export interface TranspileOptions {
  /**
   * Called for every unsupported construct (namespaces with runtime code,
   * parameter properties, `export =`, `import x = require(...)`, `<T>expr`
   * assertions, unsafe `as` erasures). The offending source stays verbatim,
   * mirroring ts-blank-space; enums are expanded instead of reported.
   */
  readonly onError?: OnError
  /**
   * Called for every recoverable condition, e.g. a define replacement
   * skipped in a JSX tag position (`{ FLAG: 'component' }` would flip the
   * component to an intrinsic string tag).
   */
  readonly onWarn?: OnWarn
  /** Parse the input as `ts` (default) or `tsx`. */
  readonly lang?: 'ts' | 'tsx'
  /**
   * Source path quoted in the diagnostics of the `SyntaxError` thrown when
   * the input cannot be parsed. The extension also selects the parse mode
   * (a `.tsx` filename enables JSX), so `lang` only synthesizes a fallback
   * name when this is omitted.
   */
  readonly filename?: string
  /**
   * Compile-time replacement of global references, esbuild-style:
   * `{ __DEV__: 'true', 'process.env.NODE_ENV': '"production"' }`.
   * Purely textual: shadowed references and writes are left untouched.
   * A replacement shorter than the span it covers is padded with trailing
   * spaces up to the span's length in UTF-16 code units (JavaScript string
   * positions), so later columns on the same line keep their positions; a
   * longer replacement still shifts them (a partial guarantee, matching
   * the whitespace-padding philosophy of the erasure itself).
   */
  readonly define?: Readonly<Record<string, string>>
}
