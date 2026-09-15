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
  describe('replacement', () => {
    it('replaces a bare global identifier', () => {
      const output = transpile('console.log(__DEV__)', { define: { __DEV__: 'true' } })
      expect(output).toContain('console.log(true   )')
      expect(output).not.toContain('__DEV__')
      expect(output).toMatchInlineSnapshot(`"console.log(true   )"`)
    })

    it('replaces a dotted global path', () => {
      const output = transpile('console.log(process.env.NODE_ENV)', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('console.log("production"        )')
      expect(output).not.toContain('NODE_ENV')
      expect(output).toMatchInlineSnapshot(`"console.log("production"        )"`)
    })

    it('replaces a string-indexed chain', () => {
      const output = transpile('console.log(process.env["NODE_ENV"])', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('console.log("production"           )')
      expect(output).toMatchInlineSnapshot(`"console.log("production"           )"`)
    })

    it('splices an entity-name value literally', () => {
      const output = transpile('console.log(flag)', { define: { flag: 'DEBUG' } })
      expect(output).toContain('console.log(DEBUG)')
      expect(output).not.toContain('flag')
      expect(output).toMatchInlineSnapshot(`"console.log(DEBUG)"`)
    })

    it('does not re-resolve an entity value against other defines', () => {
      const output = transpile('console.log(flag)', {
        define: { flag: 'DEBUG', DEBUG: 'true' },
      })
      expect(output).toContain('console.log(DEBUG)')
      expect(output).not.toContain('true')
      expect(output).toMatchInlineSnapshot(`"console.log(DEBUG)"`)
    })

    it('prefers the longest matching key', () => {
      const output = transpile('log(a.b.c)', { define: { 'a.b': '1', 'a.b.c': '2' } })
      expect(output).toContain('log(2    )')
      expect(output).not.toContain('a.b.c')
      expect(output).toMatchInlineSnapshot(`"log(2    )"`)
    })

    it('separates a numeric literal from a following member dot', () => {
      const output = transpile(
        ['log(foo.NODE_ENV)', 'log(foo.NODE_ENV.x)'].join('\n'),
        { define: { 'foo.NODE_ENV': '42' } },
      )
      expect(output).toContain('log(42          )')
      expect(output).toContain('42          .x')
      expect(output).not.toContain('42.x')
      expect(output).toMatchInlineSnapshot(`
        "log(42          )
        log(42          .x)"
      `)
    })

    it('keeps the value spelling as written', () => {
      const output = transpile(
        ['log(cfg.REV)', 'log(cfg.BIG)'].join('\n'),
        { define: { 'cfg.REV': '1e3', 'cfg.BIG': '123n' } },
      )
      expect(output).toContain('log(1e3    )')
      expect(output).toContain('log(123n   )')
      expect(output).toMatchInlineSnapshot(`
        "log(1e3    )
        log(123n   )"
      `)
    })

    it('replaces under typeof without folding it', () => {
      const output = transpile('const t = typeof __DEV__', { define: { __DEV__: 'true' } })
      expect(output).toContain('typeof true')
      expect(output).not.toContain('"boolean"')
      expect(output).toMatchInlineSnapshot(`"const t = typeof true   "`)
    })

    it('does not fold replaced branches or drop code', () => {
      const output = transpile('if (__DEV__) {\n  sideEffect()\n}', {
        define: { __DEV__: 'false' },
      })
      expect(output).toContain('if (false  )')
      expect(output).toContain('sideEffect()')
      expect(output).toMatchInlineSnapshot(`
        "if (false  ) {
          sideEffect()
        }"
      `)
    })

    it('expands a shorthand property', () => {
      const output = transpile('const o = { NODE_ENV }', { define: { NODE_ENV: '42' } })
      expect(output).toContain('NODE_ENV: 42')
      expect(output).not.toContain('{ NODE_ENV }')
      expect(output).toMatchInlineSnapshot(`"const o = { NODE_ENV: 42 }"`)
    })

    it('replaces a whole optional chain', () => {
      const output = transpile('log(a?.b.c)', { define: { 'a.b.c': '2' } })
      expect(output).toContain('log(2     )')
      expect(output).toMatchInlineSnapshot(`"log(2     )"`)
    })

    it('replaces a prefix inside an optional chain', () => {
      const output = transpile('log(x?.y.z)', { define: { x: '1' } })
      expect(output).toContain('1?.y.z')
      expect(output).toMatchInlineSnapshot(`"log(1?.y.z)"`)
    })

    it('never matches computed member accesses', () => {
      const output = transpile('log(a[b].c)', { define: { 'a.b.c': '2' } })
      expect(output).toContain('a[b].c')
      expect(output).not.toContain('log(2)')
      expect(output).toMatchInlineSnapshot(`"log(a[b].c)"`)
    })

    it('splices undefined as shadow-immune void 0', () => {
      const output = transpile('console.log(flag)', { define: { flag: 'undefined' } })
      expect(output).toContain('console.log(void 0)')
      expect(output).toMatchInlineSnapshot(`"console.log(void 0)"`)
    })

    it('replaces call targets', () => {
      const output = transpile(['__A()', '__B()'].join('\n'), {
        define: { __A: 'sideEffect', __B: 'true' },
      })
      expect(output).toContain('sideEffect()')
      expect(output).toContain('true()')
      expect(output).toMatchInlineSnapshot(`
        "sideEffect()
        true()"
      `)
    })

    it('replaces inside template interpolations', () => {
      const output = transpile('const s = `${__DEV__}`', { define: { __DEV__: 'true' } })
      expect(output).toContain('\${true   }')
      expect(output).toMatchInlineSnapshot(`"const s = \`\${true   }\`"`)
    })

    it('replaces default parameters and computed keys', () => {
      const output = transpile(
        ['function f(a = __DEV__) {}', 'const o = { [__DEV__]: 1 }'].join('\n'),
        { define: { __DEV__: 'true' } },
      )
      expect(output).toContain('a = true')
      expect(output).toContain('[true   ]: 1')
      expect(output).toMatchInlineSnapshot(`
        "function f(a = true   ) {}
        const o = { [true   ]: 1 }"
      `)
    })

    it('replaces inside as-assertions', () => {
      const output = transpile('const x = __DEV__ as any', { define: { __DEV__: 'true' } })
      expect(output).toContain('true')
      expect(output).not.toContain('__DEV__')
      expect(output).toMatchInlineSnapshot(`"const x = true   ;      "`)
    })

    it('replaces a whole chain with the optional link last', () => {
      const output = transpile('log(a.b?.c)', { define: { 'a.b.c': '2' } })
      expect(output).toContain('log(2     )')
      expect(output).toMatchInlineSnapshot(`"log(2     )"`)
    })

    it('replaces mixed dot/index optional chains', () => {
      const output = transpile('log(a[\'b\']?.[\'c\'])', { define: { 'a.b.c': '2' } })
      expect(output).toContain('log(2            )')
      expect(output).toMatchInlineSnapshot(`"log(2            )"`)
    })

    it('replaces the prefix before an optional link', () => {
      const output = transpile('log(a.b?.c)', { define: { 'a.b': '1' } })
      expect(output).toContain('1  ?.c')
      expect(output).toMatchInlineSnapshot(`"log(1  ?.c)"`)
    })

    it('replaces a member chain under delete', () => {
      const output = transpile('delete process.env.NODE_ENV', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('delete "production"')
      expect(output).toMatchInlineSnapshot(`"delete "production"        "`)
    })
  })
})
