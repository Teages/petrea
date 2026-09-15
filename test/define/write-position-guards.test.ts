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
  describe('write-position guards', () => {
    it('keeps writes to a literal-valued define untouched', () => {
      const output = transpile(['NODE_ENV = \'x\'', 'NODE_ENV++'].join('\n'), {
        define: { NODE_ENV: '42' },
      })
      expect(output).toContain('NODE_ENV = \'x\'')
      expect(output).toContain('NODE_ENV++')
      expect(output).toMatchInlineSnapshot(`
        "NODE_ENV = 'x'
        NODE_ENV++"
      `)
    })

    it('replaces a write target whose value is an entity name', () => {
      const output = transpile('flag = 1', { define: { flag: 'DEBUG' } })
      expect(output).toContain('DEBUG = 1')
      expect(output).toMatchInlineSnapshot(`"DEBUG = 1"`)
    })

    it('keeps destructuring write targets with literal values', () => {
      const output = transpile(
        [
          '({ NODE_ENV } = o);',
          '[NODE_ENV] = xs;',
          '[...NODE_ENV] = xs;',
          '[NODE_ENV = fallback] = xs;',
          '({ key: NODE_ENV } = o);',
        ].join('\n'),
        { define: { NODE_ENV: '42' } },
      )
      expect(output.match(/NODE_ENV/g)).toHaveLength(5)
      expect(output).not.toContain('42')
      expect(output).toMatchInlineSnapshot(`
        "({ NODE_ENV } = o);
        [NODE_ENV] = xs;
        [...NODE_ENV] = xs;
        [NODE_ENV = fallback] = xs;
        ({ key: NODE_ENV } = o);"
      `)
    })

    it('replaces destructuring write targets with entity values', () => {
      const output = transpile(['({ flag } = o)', '[flag] = xs'].join('\n'), {
        define: { flag: 'DEBUG' },
      })
      expect(output).toContain('({ flag: DEBUG } = o)')
      expect(output).toContain('[DEBUG] = xs')
      expect(output).toMatchInlineSnapshot(`
        "({ flag: DEBUG } = o)
        [DEBUG] = xs"
      `)
    })
  })
})
