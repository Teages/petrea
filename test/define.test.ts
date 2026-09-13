import { describe, expect, it } from 'vitest'
import { transpile } from './parity'

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
      expect(output).toContain('console.log(true)')
      expect(output).not.toContain('__DEV__')
      expect(output).toMatchInlineSnapshot(`"console.log(true)"`)
    })

    it('replaces a dotted global path', () => {
      const output = transpile('console.log(process.env.NODE_ENV)', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('console.log("production")')
      expect(output).not.toContain('NODE_ENV')
      expect(output).toMatchInlineSnapshot(`"console.log("production")"`)
    })

    it('replaces a string-indexed chain', () => {
      const output = transpile('console.log(process.env["NODE_ENV"])', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('console.log("production")')
      expect(output).toMatchInlineSnapshot(`"console.log("production")"`)
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
      expect(output).toContain('log(2)')
      expect(output).not.toContain('a.b.c')
      expect(output).toMatchInlineSnapshot(`"log(2)"`)
    })

    it('separates a numeric literal from a following member dot', () => {
      const output = transpile(
        ['log(foo.NODE_ENV)', 'log(foo.NODE_ENV.x)'].join('\n'),
        { define: { 'foo.NODE_ENV': '42' } },
      )
      expect(output).toContain('log(42)')
      expect(output).toContain('42 .x')
      expect(output).not.toContain('42.x')
      expect(output).toMatchInlineSnapshot(`
        "log(42)
        log(42 .x)"
      `)
    })

    it('keeps the value spelling as written', () => {
      const output = transpile(
        ['log(cfg.REV)', 'log(cfg.BIG)'].join('\n'),
        { define: { 'cfg.REV': '1e3', 'cfg.BIG': '123n' } },
      )
      expect(output).toContain('log(1e3)')
      expect(output).toContain('log(123n)')
      expect(output).toMatchInlineSnapshot(`
        "log(1e3)
        log(123n)"
      `)
    })

    it('replaces under typeof without folding it', () => {
      const output = transpile('const t = typeof __DEV__', { define: { __DEV__: 'true' } })
      expect(output).toContain('typeof true')
      expect(output).not.toContain('"boolean"')
      expect(output).toMatchInlineSnapshot(`"const t = typeof true"`)
    })

    it('does not fold replaced branches or drop code', () => {
      const output = transpile('if (__DEV__) {\n  sideEffect()\n}', {
        define: { __DEV__: 'false' },
      })
      expect(output).toContain('if (false)')
      expect(output).toContain('sideEffect()')
      expect(output).toMatchInlineSnapshot(`
        "if (false) {
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
      expect(output).toContain('log(2)')
      expect(output).toMatchInlineSnapshot(`"log(2)"`)
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

    it('splices undefined as a plain entity value', () => {
      const output = transpile('console.log(flag)', { define: { flag: 'undefined' } })
      expect(output).toContain('console.log(undefined)')
      expect(output).toMatchInlineSnapshot(`"console.log(undefined)"`)
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
      expect(output).toContain('\${true}')
      expect(output).toMatchInlineSnapshot(`"const s = \`\${true}\`"`)
    })

    it('replaces default parameters and computed keys', () => {
      const output = transpile(
        ['function f(a = __DEV__) {}', 'const o = { [__DEV__]: 1 }'].join('\n'),
        { define: { __DEV__: 'true' } },
      )
      expect(output).toContain('a = true')
      expect(output).toContain('[true]: 1')
      expect(output).toMatchInlineSnapshot(`
        "function f(a = true) {}
        const o = { [true]: 1 }"
      `)
    })

    it('replaces inside as-assertions', () => {
      const output = transpile('const x = __DEV__ as any', { define: { __DEV__: 'true' } })
      expect(output).toContain('true')
      expect(output).not.toContain('__DEV__')
      expect(output).toMatchInlineSnapshot(`"const x = true;      "`)
    })

    it('replaces a whole chain with the optional link last', () => {
      const output = transpile('log(a.b?.c)', { define: { 'a.b.c': '2' } })
      expect(output).toContain('log(2)')
      expect(output).toMatchInlineSnapshot(`"log(2)"`)
    })

    it('replaces mixed dot/index optional chains', () => {
      const output = transpile('log(a[\'b\']?.[\'c\'])', { define: { 'a.b.c': '2' } })
      expect(output).toContain('log(2)')
      expect(output).toMatchInlineSnapshot(`"log(2)"`)
    })

    it('replaces the prefix before an optional link', () => {
      const output = transpile('log(a.b?.c)', { define: { 'a.b': '1' } })
      expect(output).toContain('1?.c')
      expect(output).toMatchInlineSnapshot(`"log(1?.c)"`)
    })

    it('replaces a member chain under delete', () => {
      const output = transpile('delete process.env.NODE_ENV', {
        define: { 'process.env.NODE_ENV': '"production"' },
      })
      expect(output).toContain('delete "production"')
      expect(output).toMatchInlineSnapshot(`"delete "production""`)
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
      expect(output).toMatchInlineSnapshot(`
        "{
          let process = { env: { NODE_ENV: "x" } }
          console.log(process.env.NODE_ENV)
        }
        console.log("production")"
      `)
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
      expect(output).toMatchInlineSnapshot(`
        "function f(process     ) {
          return process.env.NODE_ENV
        }
        console.log("production")"
      `)
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

    it('skips references a hoisted var shadows (use before declaration)', () => {
      const output = transpile(
        [
          'function f() {',
          '  console.log(process.env.NODE_ENV)',
          '  var process = { env: {} }',
          '}',
        ].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('process.env.NODE_ENV')
      expect(output).not.toContain('"production"')
      expect(output).toMatchInlineSnapshot(`
        "function f() {
          console.log(process.env.NODE_ENV)
          var process = { env: {} }
        }"
      `)
    })

    it('skips references a catch parameter shadows', () => {
      const output = transpile(
        'try { f() } catch (process) { console.log(process.env.NODE_ENV) }',
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('process.env.NODE_ENV')
      expect(output).not.toContain('"production"')
      expect(output).toMatchInlineSnapshot(`"try { f() } catch (process) { console.log(process.env.NODE_ENV) }"`)
    })

    it('skips references a function declaration name shadows', () => {
      const output = transpile(
        ['function process() {}', 'console.log(process.env.NODE_ENV)'].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('process.env.NODE_ENV')
      expect(output).not.toContain('"production"')
      expect(output).toMatchInlineSnapshot(`
        "function process() {}
        console.log(process.env.NODE_ENV)"
      `)
    })

    it('does not treat class fields as bindings', () => {
      const output = transpile(
        ['class C {', '  process = 1', '  m() { return process.env.NODE_ENV }', '}'].join('\n'),
        { define: { 'process.env.NODE_ENV': '"production"' } },
      )
      expect(output).toContain('return "production"')
      expect(output).toMatchInlineSnapshot(`
        "class C {
          process = 1
          m() { return "production" }
        }"
      `)
    })

    it('erases a declare const and replaces its references', () => {
      const output = transpile(
        ['declare const NODE_ENV: string', 'console.log(NODE_ENV)'].join('\n'),
        { define: { NODE_ENV: '"production"' } },
      )
      expect(output).not.toContain('declare')
      expect(output).toContain('console.log("production")')
      expect(output).not.toContain('NODE_ENV')
      expect(output).toMatchInlineSnapshot(`
        "                              
        console.log("production")"
      `)
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
      expect(output).toContain('({ DEBUG } = o)')
      expect(output).toContain('[DEBUG] = xs')
      expect(output).toMatchInlineSnapshot(`
        "({ DEBUG } = o)
        [DEBUG] = xs"
      `)
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
      expect(output).toMatchInlineSnapshot(`"var  E; (function (E) { E[E["A"] = true] = "A" })(E || (E = {}));"`)
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

  describe('edge interactions', () => {
    it('for-of write target with literal value is kept', () => {
      const output = transpile('for (NODE_ENV of xs) {}', { define: { NODE_ENV: '42' } })
      expect(output).toContain('NODE_ENV of xs')
    })

    it('for-of write target with entity value is replaced', () => {
      const output = transpile('for (flag of xs) {}', { define: { flag: 'DEBUG' } })
      expect(output).toContain('DEBUG of xs')
    })

    it('works on the UTF-16 path (BOM input)', () => {
      const output = transpile('﻿console.log(__DEV__)', { define: { __DEV__: 'true' } })
      expect(output).toContain('console.log(true)')
    })

    it('kept namespaces stay verbatim (no substitution inside)', () => {
      const reports: string[] = []
      const output = transpile('namespace N {\n  export const x = __DEV__\n}', {
        define: { __DEV__: 'true' },
        onError: node => reports.push(node.type),
      })
      expect(output).toBe('namespace N {\n  export const x = __DEV__\n}')
      expect(reports).toContain('TSModuleDeclaration')
    })

    it('is shadowed by a runtime namespace name', () => {
      const output = transpile('namespace N { export const a = 1 }\nconsole.log(N)', {
        define: { N: '42' },
        onError: () => {},
      })
      expect(output).toContain('console.log(N)')
    })

    it('is not shadowed by a declare namespace', () => {
      const output = transpile('declare namespace N {}\nconsole.log(N)', { define: { N: '42' } })
      expect(output).toContain('console.log(42)')
    })
  })
})
