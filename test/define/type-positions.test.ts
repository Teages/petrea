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
  describe('type positions', () => {
    it('never touches erased type positions', () => {
      const input = 'const a: typeof __DEV__ = 1'
      const output = transpile(input, { define: { __DEV__: 'true' } })
      expect(output).not.toContain('true')
      expect(output.length).toBe(input.length)
      expect(output).toMatchInlineSnapshot(`"const a                 = 1"`)
    })
  })
})
