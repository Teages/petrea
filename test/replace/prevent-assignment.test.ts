import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

// `preventAssignment` ports the reference plugins' lookahead `(?!\s*=[^=])`
// exactly: comparisons and compound assignments pass — the same footgun the
// reference plugins have, pinned here.
describe('replace', () => {
  describe('preventAssignment', () => {
    const options = { replace: { KEY: '1' }, replaceOptions: { preventAssignment: true } }

    it('blocks an assignment', () => {
      expect(transpile('KEY = x', options)).toBe('KEY = x')
    })

    it('blocks an arrow function', () => {
      expect(transpile('KEY => x', options)).toBe('KEY => x')
    })

    it('blocks across whitespace before the equals sign', () => {
      expect(transpile('KEY\t=  x', options)).toBe('KEY\t=  x')
    })

    it('walks the js regex \\s class, not unicode White_Space', () => {
      // U+FEFF and U+00A0 are js whitespace and join the run; U+0085 is
      // White_Space but not js `\\s`, so the replace proceeds and `1 = x`
      // fails the strict gate loudly
      expect(transpile('KEY \uFEFF= x', options)).toBe('KEY \uFEFF= x')
      expect(transpile('KEY\u00A0= x', options)).toBe('KEY\u00A0= x')
      expect(() => transpile('KEY\u0085= x', options)).toThrow(
        'failed to parse input.ts (after replace)',
      )
    })

    it('does not block a comparison', () => {
      expect(transpile('KEY == x', options)).toBe('1 == x')
      expect(transpile('KEY === x', options)).toBe('1 === x')
    })

    it('does not block a compound assignment (rollup parity)', () => {
      // the replaced `1 += x` is not valid JavaScript, so the transpile
      // fails loudly
      expect(() => transpile('KEY += x', options)).toThrow(
        'failed to parse input.ts (after replace)',
      )
    })

    it('blocks a declaration prefix', () => {
      expect(transpile('let KEY;', options)).toBe('let KEY;')
      expect(transpile('var KEY = 1', options)).toBe('var KEY = 1')
      expect(transpile('const KEY = 1', options)).toBe('const KEY = 1')
      expect(transpile('var\nKEY = 1', options)).toBe('var\nKEY = 1')
      // only a whole keyword counts (the probe lives inside a string literal
      // to stay parseable)
      expect(transpile('const s = "xconst KEY"', options)).toBe('const s = "xconst 1"')
    })

    it('replaces everything when off (default)', () => {
      // a value that breaks the statement (`1 = x`, `let 1;`) fails the
      // transpile loudly
      expect(() => transpile('KEY = x', { replace: { KEY: '1' } })).toThrow(
        'failed to parse input.ts (after replace)',
      )
      expect(() => transpile('let KEY;', { replace: { KEY: '1' } })).toThrow(
        'failed to parse input.ts (after replace)',
      )
      expect(transpile('KEY + x', { replace: { KEY: '1' } })).toBe('1 + x')
      expect(transpile('log(KEY)', { replace: { KEY: '1' } })).toBe('log(1)')
    })
  })
})
