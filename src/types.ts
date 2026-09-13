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

export interface TranspileOptions {
  /**
   * Called for every unsupported construct (namespaces with runtime code,
   * parameter properties, `export =`, `import x = require(...)`, `<T>expr`
   * assertions, unsafe `as` erasures). The offending source stays verbatim,
   * mirroring ts-blank-space; enums are expanded instead of reported.
   */
  readonly onError?: OnError
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
   * Compile-time replacement of global references, esbuild-style. Keys are
   * dot-separated global paths (`__DEV__`, `process.env.NODE_ENV`); values
   * are the source text of a primitive literal (`true`, `42`, `'"production"'`,
   * `123n`, `null`) or an entity name (`DEBUG`, `undefined`) spliced into
   * every unshadowed reference. References that a local binding shadows, and
   * writes to literal-valued defines, are left untouched.
   *
   * Not yet implemented: the option is accepted but currently ignored — the
   * output is identical to a transpile without it.
   */
  readonly define?: Readonly<Record<string, string>>
}
