import { describe, expect, it } from 'vitest'
import { transpile } from './parity'

/**
 * `define` follows a TDD arrangement: each test first asserts the key
 * behavioral points, then pins the full output with an inline snapshot.
 * Until substitution is implemented the "replaced" key points fail (the
 * option is accepted but ignored), so snapshots only generate — on a local,
 * non-CI `vitest run` — once the key points pass. The "left untouched"
 * cases already pass today; their snapshots record current output and must
 * survive the implementation unchanged.
 *
 * Expected behavior is esbuild-verified (see the design notes): unshadowed
 * global references are textually replaced; no constant folding, no dead-code
 * elimination, no `typeof` folding.
 */
describe('define', () => {
  describe('replacement', () => {
    it('replaces a bare global identifier', () => {
      const output = transpile('console.log(__DEV__)', { define: { __DEV__: 'true' } })
      expect(output).toContain('console.log(true)')
      expect(output).not.toContain('__DEV__')
      expect(output).toMatchInlineSnapshot()
    })

    it('replaces a dotted global path', () => {
      const output = transpile('console.log(process.env.NODE_ENV)', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('console.log("production")')
      expect(output).not.toContain('NODE_ENV')
      expect(output).toMatchInlineSnapshot()
    })

    it('replaces a string-indexed chain', () => {
      const output = transpile('console.log(process.env["NODE_ENV"])', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('console.log("production")')
      expect(output).toMatchInlineSnapshot()
    })

    it('splices an entity-name value literally', () => {
      const output = transpile('console.log(flag)', { define: { flag: 'DEBUG' } })
      expect(output).toContain('console.log(DEBUG)')
      expect(output).not.toContain('flag')
      expect(output).toMatchInlineSnapshot()
    })

    it('does not re-resolve an entity value against other defines', () => {
      const output = transpile('console.log(flag)', {
        define: { flag: 'DEBUG', DEBUG: 'true' },
      })
      expect(output).toContain('console.log(DEBUG)')
      expect(output).not.toContain('true')
      expect(output).toMatchInlineSnapshot()
    })

    it('prefers the longest matching key', () => {
      const output = transpile('log(a.b.c)', { define: { 'a.b': '1', 'a.b.c': '2' } })
      expect(output).toContain('log(2)')
      expect(output).not.toContain('a.b.c')
      expect(output).toMatchInlineSnapshot()
    })

    it('separates a numeric literal from a following member dot', () => {
      const output = transpile(
        ['log(foo.NODE_ENV)', 'log(foo.NODE_ENV.x)'].join('\n'),
        { define: { 'foo.NODE_ENV': '42' } },
      )
      expect(output).toContain('log(42)')
      expect(output).toContain('42 .x')
      expect(output).not.toContain('42.x')
      expect(output).toMatchInlineSnapshot()
    })

    it('keeps the value spelling as written', () => {
      const output = transpile(
        ['log(cfg.REV)', 'log(cfg.BIG)'].join('\n'),
        { define: { 'cfg.REV': '1e3', 'cfg.BIG': '123n' } },
      )
      expect(output).toContain('log(1e3)')
      expect(output).toContain('log(123n)')
      expect(output).toMatchInlineSnapshot()
    })

    it('replaces under typeof without folding it', () => {
      const output = transpile('const t = typeof __DEV__', { define: { __DEV__: 'true' } })
      expect(output).toContain('typeof true')
      expect(output).not.toContain('"boolean"')
      expect(output).toMatchInlineSnapshot()
    })

    it('does not fold replaced branches or drop code', () => {
      const output = transpile('if (__DEV__) {\n  sideEffect()\n}', {
        define: { __DEV__: 'false' },
      })
      expect(output).toContain('if (false)')
      expect(output).toContain('sideEffect()')
      expect(output).toMatchInlineSnapshot()
    })

    it('expands a shorthand property', () => {
      const output = transpile('const o = { NODE_ENV }', { define: { NODE_ENV: '42' } })
      expect(output).toContain('NODE_ENV: 42')
      expect(output).not.toContain('{ NODE_ENV }')
      expect(output).toMatchInlineSnapshot()
    })

    it('replaces a whole optional chain', () => {
      const output = transpile('log(a?.b.c)', { define: { 'a.b.c': '2' } })
      expect(output).toContain('log(2)')
      expect(output).toMatchInlineSnapshot()
    })

    it('replaces a prefix inside an optional chain', () => {
      const output = transpile('log(x?.y.z)', { define: { x: '1' } })
      expect(output).toContain('1?.y.z')
      expect(output).toMatchInlineSnapshot()
    })

    it('never matches computed member accesses', () => {
      const output = transpile('log(a[b].c)', { define: { 'a.b.c': '2' } })
      expect(output).toContain('a[b].c')
      expect(output).not.toContain('log(2)')
      expect(output).toMatchInlineSnapshot(`"log(a[b].c)"`)
    })

    it('replaces a member chain under delete', () => {
      const output = transpile('delete process.env.NODE_ENV', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('delete "production"')
      expect(output).toMatchInlineSnapshot()
    })
  })

  describe('shadowing', () => {
    it('skips references a block-scoped binding shadows', () => {
      const output = transpile(
        [
          '{',
          '  let process = { env: { NODE_ENV: "x" } }',
          '  console.log(process.env.NODE_ENV)',
          '}',
          'console.log(process.env.NODE_ENV)',
        ].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('process.env.NODE_ENV')
      expect(output.indexOf('process.env.NODE_ENV')).toBeLessThan(output.indexOf('"production"'))
      expect(output).toMatchInlineSnapshot()
    })

    it('skips references a parameter shadows', () => {
      const output = transpile(
        [
          'function f(process: any) {',
          '  return process.env.NODE_ENV',
          '}',
          'console.log(process.env.NODE_ENV)',
        ].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('process.env.NODE_ENV')
      expect(output.indexOf('process.env.NODE_ENV')).toBeLessThan(output.indexOf('"production"'))
      expect(output).toMatchInlineSnapshot()
    })

    it('skips references an import shadows', () => {
      const output = transpile(
        ['import process from "node:process"', 'console.log(process.env.NODE_ENV)'].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('process.env.NODE_ENV')
      expect(output).not.toContain('"production"')
      expect(output).toMatchInlineSnapshot(`
        "import process from "node:process"
        console.log(process.env.NODE_ENV)"
      `)
    })

    it('does not treat class fields as bindings', () => {
      const output = transpile(
        ['class C {', '  process = 1', '  m() { return process.env.NODE_ENV }', '}'].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('return "production"')
      expect(output).toMatchInlineSnapshot()
    })

    it('erases a declare const and replaces its references', () => {
      const output = transpile(
        ['declare const NODE_ENV: string', 'console.log(NODE_ENV)'].join('\n'),
        { define: { NODE_ENV: '"production"' } },
      )
      expect(output).not.toContain('declare')
      expect(output).toContain('console.log("production")')
      expect(output).not.toContain('NODE_ENV')
      expect(output).toMatchInlineSnapshot()
    })
  })

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
      expect(output).toMatchInlineSnapshot()
    })
  })

  describe('type positions', () => {
    it('never touches erased type positions', () => {
      const input = 'const a: typeof __DEV__ = 1'
      const output = transpile(input, { define: { __DEV__: 'true' } })
      expect(output).not.toContain('true')
      expect(output.length).toBe(input.length)
      expect(output).toMatchInlineSnapshot(`"const a                 = 1"`)
    })
  })

  describe('invariants', () => {
    it('preserves line count and untouched lines', () => {
      const output = transpile(
        ['const mode = process.env.NODE_ENV', 'const other = 2'].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('"production"')
      expect(output).toContain('const other = 2')
      expect(output.split('\n')).toHaveLength(2)
      expect(output).toMatchInlineSnapshot()
    })

    it('inlines defines inside enum member initializers', () => {
      const output = transpile('enum E { A = __DEV__ }', { define: { __DEV__: 'true' } })
      expect(output).not.toContain('__DEV__')
      const E = new Function(`${output}; return E`)() as Record<string, unknown>
      expect(E.A).toBe(true)
      expect(output).toMatchInlineSnapshot()
    })

    it('is a no-op with an empty define map', () => {
      const input = 'const a: number = 1'
      expect(transpile(input, { define: {} })).toBe(transpile(input))
    })
  })

  describe('validation', () => {
    it('rejects malformed keys', () => {
      expect(() => transpile('console.log(1)', { define: { '': '1' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { 'a..b': '1' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { 'a b': '1' } })).toThrow()
    })

    it('rejects this and import.meta roots', () => {
      expect(() => transpile('console.log(1)', { define: { 'this.x': '1' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { 'import.meta.env': '1' } })).toThrow()
    })

    it('rejects values that are not literals or entity names', () => {
      expect(() => transpile('console.log(1)', { define: { x: '1 + 2' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { x: 'foo()' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { x: '{ a: 1 }' } })).toThrow()
      expect(() => transpile('console.log(1)', { define: { x: '' } })).toThrow()
    })
  })
})
