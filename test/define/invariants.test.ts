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
  describe('invariants', () => {
    it('preserves line count and untouched lines', () => {
      const output = transpile(
        ['const mode = process.env.NODE_ENV', 'const other = 2'].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('"production"')
      expect(output).toContain('const other = 2')
      expect(output.split('\n')).toHaveLength(2)
      expect(output).toMatchInlineSnapshot(`
        "const mode = "production"        
        const other = 2"
      `)
    })

    it('inlines defines inside enum member initializers', () => {
      const output = transpile('enum E { A = __DEV__ }', { define: { __DEV__: 'true' } })
      expect(output).not.toContain('__DEV__')
      const E = new Function(`${output}; return E`)() as Record<string, unknown>
      expect(E.A).toBe(true)
      expect(output).toMatchInlineSnapshot(`"var  E; (function (E) { E[E["A"] = true   ] = "A" })(E || (E = {}));"`)
    })

    it('qualifies an entity value captured as an enum member', () => {
      // esbuild folds this to the member's value (123); the textual splice
      // instead qualifies the root so the read resolves through the enum
      // object at runtime — same value, no folding machinery
      const output = transpile('enum E { B = 123, C = d }', { define: { d: 'B' } })
      expect(output).toContain('= E.B')
      const E = new Function(`${output}; return E`)() as Record<string, unknown>
      expect(E.C).toBe(123)
      expect(output).toMatchInlineSnapshot(`"var  E; (function (E) { E[E["B"] = 123] = "B"; E[E["C"] = E.B] = "C" })(E || (E = {}));"`)
    })

    it('is a no-op with an empty define map', () => {
      const input = 'const a: number = 1'
      expect(transpile(input, { define: {} })).toBe(transpile(input))
    })
  })
})
