import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

// `replace` is the pipeline's one pre-parse text rewrite: with at least one
// hit the rewritten text is what the pipeline parses (under a total gate), so
// every downstream pass consumes only the replaced program; with none the
// transpile is byte-for-byte replace-free. The scanner consults no parse, so
// even an unparseable original rewrites.
describe('replace', () => {
  describe('pipeline', () => {
    it('runs before the erasure, which blanks its output', () => {
      expect(transpile('const a: T = KEY;', { replace: { KEY: '1' } })).toBe('const a    = 1;')
      expect(transpile('type T = KEY;', { replace: { KEY: '1' } })).toBe('           ')
    })

    it('fails the transpile loudly when a replacement breaks the syntax', () => {
      // the rewritten text is what parses next; a same-shape value stays
      // valid TypeScript and the erasure blanks it as usual
      expect(() => transpile('let x: T = 1;', { replace: { 'T =': ')' } })).toThrow(
        'failed to parse input.ts (after replace)',
      )
      expect(transpile('let x: T = 1;', { replace: { 'T =': 'U =' } })).toBe('let x    = 1;')
    })

    it('fails the transpile loudly when a replacement makes a recovered program', () => {
      // oxc recovers a program for a top-level return in a module (the
      // ordinary gate would pass it), but the post-replace gate is total
      expect(() => transpile('FLAG;', { replace: { FLAG: 'return' } })).toThrow(
        'failed to parse input.ts (after replace)',
      )
      // the same recovered shape the user wrote themselves still transpiles
      expect(transpile('return;\nlog(1);', { replace: { ABSENT: '1' } })).toBe('return;\nlog(1);')
    })

    it('rewrites unparseable originals — the placeholder flow (rollup semantics)', () => {
      expect(transpile('const x = @VALUE@;', { replace: { '@VALUE@': '\'1\'' } })).toBe(
        `const x = '1';`,
      )
      // nothing matched: the ordinary gate rejects the original parse (the
      // error names no replace)
      expect(() => transpile('const x = @VALUE@;', { replace: { ABSENT: '1' } })).toThrow(
        'failed to parse input.ts:',
      )
    })

    it('renames an enum self-consistently (declaration and references together)', () => {
      expect(transpile('enum E { A }\nlog(E);', { replace: { E: 'F' } })).toBe(
        'var  F; (function (F) { F[F["A"] = 0] = "A" })(F || (F = {}));\nlog(F);',
      )
      expect(transpile('enum E { A }\nlog(E.A);', { replace: { A: 'B' } })).toBe(
        'var  E; (function (E) { E[E["B"] = 0] = "B" })(E || (E = {}));\nlog(E.B);',
      )
    })

    it('lets a replacement make an enum invalid and fails loudly', () => {
      // `E → 0` rewrites the declaration's name too: `enum 0` is not TypeScript
      expect(() =>
        transpile('enum E { A }\nif (E) { yes(); }', { replace: { E: '0' } }),
      ).toThrow('failed to parse input.ts (after replace)')
      // `if (F)` does not fold: an identifier is no literal
      expect(transpile('enum E { A }\nif (E) { yes(); }', { replace: { E: 'F' } })).toBe(
        'var  F; (function (F) { F[F["A"] = 0] = "A" })(F || (F = {}));\nif (F) { yes(); }',
      )
    })

    it('feeds the enum pipeline consistently', () => {
      expect(transpile('const n = 1;\nenum E { A = n }', { replace: { 1: '2' } })).toBe(
        'const n = 2;\nvar  E; (function (E) { E[E["A"] = 2] = "A" })(E || (E = {}));',
      )
      expect(transpile('enum E { A = 1, B = A + f() }', { replace: { A: 'X' } })).toBe(
        'var  E; (function (E) { E[E["X"] = 1] = "X"; E[E["B"] = E.X + f()] = "B" })(E || (E = {}));',
      )
      expect(transpile('enum E { A = "KEY" }', { replace: { KEY: '1' } })).toBe(
        'var  E; (function (E) { E["A"] = "1" })(E || (E = {}));',
      )
    })

    it('applies inside kept-verbatim unsupported constructs (rollup semantics)', () => {
      const reports: Array<{ type: string }> = []
      const output = transpile('namespace N { export const x = KEY; }', {
        replace: { KEY: '1' },
        onError: (report) => {
          reports.push(report)
        },
      })
      expect(output).toBe('namespace N { export const x = 1; }')
      expect(reports).toHaveLength(1)
    })

    it('reuses the original-text pipeline when nothing matched', () => {
      const input = 'const a: T = KEY;\nlet b = FLAG as string;'
      expect(transpile(input, { replace: { ABSENT: '1' } })).toBe(transpile(input))
    })

    it('builds correctly when a later edit sorts before another edit', () => {
      const input = 'const a: T = KEY;\nlet b = FLAG as string;'
      const output = transpile(input, { replace: { KEY: '1', FLAG: '2' } })
      expect(output).toMatchInlineSnapshot(`
        "const a    = 1;
        let b = 2          ;"
      `)
    })
  })
})
