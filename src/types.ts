/**
 * Information about a TypeScript-only construct that has runtime semantics and
 * therefore cannot be erased. Positions are character offsets into the text
 * the pipeline parsed — the input, or the replaced text when `replace`
 * matched.
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
   * Compile-time plain-text replacement with `@rollup/plugin-replace`/rolldown
   * semantics — keys match verbatim (also inside strings and comments) and
   * values splice verbatim before the pipeline parses, so output positions
   * survive only where the splice is length-equal.
   *
   * @example
   * ```ts
   * await transpile(`const mode = process.env.NODE_ENV`, {
   *   replace: { 'process.env.NODE_ENV': '"production"' },
   * })
   * ```
   */
  readonly replace?: Readonly<Record<string, string | number>>
  /** Options for {@link TranspileOptions.replace}. */
  readonly replaceOptions?: {
    /**
     * Skip matches that look like an assignment or a declaration (`KEY = x`,
     * `KEY => x`, `const KEY`); comparisons and compound assignments still
     * replace, matching the reference plugins.
     */
    readonly preventAssignment?: boolean
    /** Also replace `typeof` prefixes of dotted keys (`a.b.c` → `typeof a`, `typeof a.b`) with `"object"`. */
    readonly objectGuards?: boolean
  }
}
