import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

/**
 * `define` tests pair key behavioral points with an inline snapshot of the
 * full output: the assertions name the contract, the snapshot pins the exact
 * text (reviewed by hand, regenerated only by a local non-CI `vitest run`
 * after a deliberate behavior change). Expected behavior is esbuild-verified:
 * unshadowed global references are textually replaced; no constant folding,
 * no dead-code elimination, no `typeof` folding.
 */
describe('define', () => {
  describe('validation', () => {
    it('rejects malformed keys', () => {
      expect(() => transpile('console.log(1)', { define: { '': '1' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { 'a..b': '1' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { 'a b': '1' } })).toThrow()
    })

    it('rejects values that are not literals or entity names', () => {
      expect(() => transpile('console.log(1)', { define: { x: '1 + 2' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { x: 'foo()' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { x: '{ a: 1 }' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { x: '' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { x: '+1' } })).toThrow()
    })
  })

  /**
   * Cases migrated from esbuild's own suites (internal/bundler_tests and
   * scripts/js-api-tests.js), with expectations re-derived for a
   * position-preserving stripper: identical replacements, but no folding
   * (`typeof` stays), no lowering (top-level `this` in ESM stays, parens
   * stay), no injection (compound values are rejected).
   */
})
