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
      expect(output).toContain('({ flag: DEBUG } = o)')
      expect(output).toContain('[DEBUG] = xs')
      expect(output).toMatchInlineSnapshot(`
        "({ flag: DEBUG } = o)
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
  describe('esbuild parity', () => {
    it('replaces top-level this and its chains, skipping nested this (TestDefineThis)', () => {
      const define = { 'this': '1', 'this.foo': '2', 'this.foo.bar': '3' } as Record<string, string>
      const block = [
        'ok(',
        '  this, this.foo, this.foo.bar,',
        '  this.foo.baz,',
        '  this.bar,',
        ')',
      ].join('\n')
      const output = transpile([block, '(() => {', block, '})()', '(function () {', block.replace(/ok/g, 'no'), '})()'].join('\n'), { define })
      expect(output).toContain('2 .baz')
      expect(output).toContain('1 .bar')
      // only the function-nested block keeps its `this` forms (five of them)
      expect(output.match(/\bthis\b/g)).toHaveLength(5)
      expect(output).toMatchInlineSnapshot(`
        "ok(
          1, 2, 3,
          2 .baz,
          1 .bar,
        )
        (() => {
        ok(
          1, 2, 3,
          2 .baz,
          1 .bar,
        )
        })()
        (function () {
        no(
          this, this.foo, this.foo.bar,
          this.foo.baz,
          this.bar,
        )
        })()"
      `)
    })

    it('replaces import.meta chains with prefix matching (TestDefineImportMeta)', () => {
      const output = transpile(
        [
          'console.log(',
          '  import.meta, import.meta.foo, import.meta.foo.bar,',
          '  import.meta.foo.baz,',
          '  import.meta.bar,',
          ')',
        ].join('\n'),
        { define: { 'import.meta': '1', 'import.meta.foo': '2', 'import.meta.foo.bar': '3' } },
      )
      expect(output).toContain('2 .baz')
      expect(output).toContain('1 .bar')
      expect(output).not.toContain('import.meta')
      expect(output).toMatchInlineSnapshot(`
        "console.log(
          1, 2, 3,
          2 .baz,
          1 .bar,
        )"
      `)
    })

    it('replaces this and import.meta as values (defineThis / defineImportMetaESM)', () => {
      const asValue = transpile('console.log(a, b); export {}', {
        define: { a: 'this', b: 'this.foo' },
      })
      expect(asValue).toContain('console.log(this, this.foo)')
      expect(asValue).toMatchInlineSnapshot(`"console.log(this, this.foo); export {}"`)

      const metaValue = transpile('console.log(a, b); export {}', {
        define: { a: 'import.meta', b: 'import.meta.foo' },
      })
      expect(metaValue).toContain('console.log(import.meta, import.meta.foo)')
      expect(metaValue).toMatchInlineSnapshot(`"console.log(import.meta, import.meta.foo); export {}"`)
    })

    it('matches bracket-spelled keys against every chain spelling (defineQuotedPropertyName)', () => {
      const forms = 'foo(x[\'y\'].z, x.y[\'z\'], x[\'y\'][\'z\'])'
      for (const key of ['x.y.z', 'x["y"].z', 'x.y["z"]', 'x["y"][\'z\']']) {
        expect(transpile(forms, { define: { [key]: 'true' } })).toContain('foo(true, true, true)')
      }
      const metaForms = 'foo(import.meta[\'y\'].z, import.meta.y[\'z\'], import.meta[\'y\'][\'z\'])'
      for (const key of ['import.meta["y"].z', 'import.meta.y["z"]', 'import.meta["y"]["z"]']) {
        expect(transpile(metaForms, { define: { [key]: 'true' } })).toContain('foo(true, true, true)')
      }
      expect(
        transpile('foo(process.env[\'SOME-TEST-VAR\'])', {
          define: { 'process.env["SOME-TEST-VAR"]': 'true' },
        }),
      ).toContain('foo(true)')
    })

    it('matches all four spellings of the NODE_ENV chain (defineProcessEnvNodeEnv)', () => {
      const define = { 'process.env.NODE_ENV': '"something"' }
      for (const form of [
        'process.env.NODE_ENV',
        'process.env[\'NODE_ENV\']',
        'process[\'env\'].NODE_ENV',
        'process[\'env\'][\'NODE_ENV\']',
      ]) {
        const output = transpile(`console.log(${form})`, { define })
        expect(output).toContain('console.log("something")')
      }
    })

    it('splices built-in constant names without folding typeof (defineBuiltInConstants)', () => {
      const output = transpile('console.log([typeof a, typeof b, typeof c, typeof d, typeof e])', {
        define: { a: 'NaN', b: 'Infinity', c: 'undefined', d: 'something', e: 'null' },
      })
      expect(output).toContain('typeof NaN')
      expect(output).toContain('typeof void 0')
      expect(output).toContain('typeof null')
      expect(output).not.toContain('"number"')
      expect(output).toMatchInlineSnapshot(`"console.log([typeof NaN, typeof Infinity, typeof void 0, typeof something, typeof null])"`)
    })

    it('splices BigInt values verbatim (defineBigInt)', () => {
      const output = transpile('console.log(a)', { define: { a: '0n' } })
      expect(output).toContain('console.log(0n)')
      expect(output).toMatchInlineSnapshot(`"console.log(0n)"`)
    })

    it('replaces the full optional-chain matrix (TestDefineOptionalChain)', () => {
      const output = transpile(
        [
          'console.log([',
          '  a.b.c,',
          '  a?.b.c,',
          '  a.b?.c,',
          '], [',
          '  a[\'b\'][\'c\'],',
          '  a?.[\'b\'][\'c\'],',
          '  a[\'b\']?.[\'c\'],',
          '], [',
          '  a[b][c],',
          '  a?.[b][c],',
          '  a[b]?.[c],',
          '])',
        ].join('\n'),
        { define: { 'a.b.c': '1' } },
      )
      expect(output.match(/ {2}1,/g)).toHaveLength(6)
      expect(output).toContain('a[b][c],')
      expect(output).toContain('a?.[b][c],')
      expect(output).toContain('a[b]?.[c],')
      expect(output).toMatchInlineSnapshot(`
        "console.log([
          1,
          1,
          1,
        ], [
          1,
          1,
          1,
        ], [
          a[b][c],
          a?.[b][c],
          a[b]?.[c],
        ])"
      `)
    })

    it('substitutes inside every optional/call/delete form (Issue3551, identifier define)', () => {
      const output = transpile(
        [
          'x?.y.z;',
          '(x?.y).z;',
          'x?.y["z"];',
          '(x?.y)["z"];',
          'x?.y();',
          '(x?.y)();',
          'x?.y.z();',
          '(x?.y).z();',
          'x?.y["z"]();',
          '(x?.y)["z"]();',
          'delete x?.y.z;',
          'delete (x?.y).z;',
          'delete x?.y["z"];',
          'delete (x?.y)["z"];',
        ].join('\n'),
        { define: { x: '1' } },
      )
      expect(output).toContain('1?.y.z;')
      expect(output).toContain('(1?.y)();')
      expect(output).toContain('delete 1?.y.z;')
      expect(output).not.toContain(' x')
      expect(output).toMatchInlineSnapshot(`
        "1?.y.z;
        (1?.y).z;
        1?.y["z"];
        (1?.y)["z"];
        1?.y();
        (1?.y)();
        1?.y.z();
        (1?.y).z();
        1?.y["z"]();
        (1?.y)["z"]();
        delete 1?.y.z;
        delete (1?.y).z;
        delete 1?.y["z"];
        delete (1?.y)["z"];"
      `)
    })

    it('substitutes the defined prefix of optional/call/delete chains (Issue3551, dot define)', () => {
      const output = transpile(
        [
          'a?.b.c;',
          '(a?.b).c;',
          'a?.b["c"];',
          '(a?.b)["c"];',
          'a?.b();',
          '(a?.b)();',
          'a?.b.c();',
          '(a?.b).c();',
          'a?.b["c"]();',
          '(a?.b)["c"]();',
          'delete a?.b.c;',
          'delete (a?.b).c;',
          'delete a?.b["c"];',
          'delete (a?.b)["c"];',
        ].join('\n'),
        { define: { 'a.b': '1' } },
      )
      expect(output).toContain('1 .c;')
      expect(output).toContain('1["c"];')
      expect(output).toContain('1();')
      expect(output).toContain('(1).c;')
      expect(output).toContain('delete 1 .c;')
      expect(output).not.toContain('a?.b')
      expect(output).toMatchInlineSnapshot(`
        "1 .c;
        (1).c;
        1["c"];
        (1)["c"];
        1();
        (1)();
        1 .c();
        (1).c();
        1["c"]();
        (1)["c"]();
        delete 1 .c;
        delete (1).c;
        delete 1["c"];
        delete (1)["c"];"
      `)
    })

    it('never re-resolves forwarded entity values (Issue2407)', () => {
      const output = transpile(['a.b()', 'x.y()'].join('\n'), {
        define: { 'a.b': 'b.c', 'b.c': 'c.a', 'c.a': 'a.b', 'x.y': 'y' },
      })
      expect(output).toContain('b.c()')
      expect(output).toContain('y()')
      expect(output).toMatchInlineSnapshot(`
        "b.c()
        y()"
      `)
    })

    it('detaches the receiver when an identifier call becomes a method call', () => {
      const output = transpile(
        ['flag()', 'flag`tpl`', 'flag?.()', 'new flag()', 'notFlag()'].join('\n'),
        { define: { flag: 'obj.method', notFlag: 'obj.also' } },
      )
      expect(output).toContain('(0, obj.method)()')
      expect(output).toContain('(0, obj.method)`tpl`')
      expect(output).toContain('(0, obj.method)?.()')
      expect(output).toContain('new obj.method()')
      expect(output).toMatchInlineSnapshot(`
        "(0, obj.method)()
        (0, obj.method)\`tpl\`
        (0, obj.method)?.()
        new obj.method()
        (0, obj.also)()"
      `)
    })

    it('keeps a receiver call when a member-call define stays a member call', () => {
      // `a.b()` already bound `a` as the receiver; splicing another member
      // expression keeps that shape — esbuild's TargetWasOriginallyPropertyAccess
      const output = transpile('a.b()', { define: { 'a.b': 'obj.method' } })
      expect(output).toContain('obj.method()')
      expect(output).not.toContain('(0,')
      expect(output).toMatchInlineSnapshot(`"obj.method()"`)
    })

    it('expands destructuring shorthand so the read property stays the key', () => {
      const output = transpile('({ flag } = obj); ({ flag: keep } = obj);', {
        define: { flag: 'DEBUG' },
      })
      expect(output).toContain('({ flag: DEBUG } = obj);')
      expect(output).toContain('({ flag: keep } = obj);')
      expect(output).toMatchInlineSnapshot(`"({ flag: DEBUG } = obj); ({ flag: keep } = obj);"`)
    })

    it('drops trailing trivia from value text', () => {
      const output = transpile('const x = FLAG / 2', { define: { FLAG: '1 //c' } })
      expect(output).toContain('1 / 2')
      expect(output).not.toContain('//c')
      expect(output).toMatchInlineSnapshot(`"const x = 1 / 2"`)
    })

    it('never emits a directive from a statement-position string', () => {
      const output = transpile(['FLAG', 'const a = 1'].join('\n'), {
        define: { FLAG: '"use strict"' },
      })
      expect(output).toContain('("use strict")')
      expect(output).toMatchInlineSnapshot(`
        "("use strict")
        const a = 1"
      `)
    })

    it('replaces undefined even where a parameter shadows the name', () => {
      const output = transpile('function f(undefined) { return FLAG }', {
        define: { FLAG: 'undefined' },
      })
      expect(output).toContain('return void 0')
      expect(output).toMatchInlineSnapshot(`"function f(undefined) { return void 0 }"`)
    })

    it('reads a local through an entity value like esbuild (NaN shadow)', () => {
      // esbuild resolves the entity through its symbol table too: a local
      // binding of the same name wins
      const output = transpile('function f(NaN) { return FLAG }', {
        define: { FLAG: 'NaN' },
      })
      expect(output).toContain('return NaN')
      expect(output).toMatchInlineSnapshot(`"function f(NaN) { return NaN }"`)
    })

    it('parenthesizes void 0 before a following member (audit round 2)', () => {
      const output = transpile('log(FLAG.x); log(FLAG);', { define: { FLAG: 'undefined' } })
      expect(output).toContain('log((void 0).x);')
      expect(output).toContain('log(void 0);')
      expect(output).toMatchInlineSnapshot(`"log((void 0).x); log(void 0);"`)
    })

    it('keeps an undefined-rooted chain as (void 0).rest (audit round 2)', () => {
      const output = transpile('log(FLAG); log(FLAG())', { define: { FLAG: 'undefined.x' } })
      expect(output).toContain('log((void 0).x);')
      expect(output).toContain('(0, (void 0).x)()')
      expect(output).toMatchInlineSnapshot(`"log((void 0).x); log((0, (void 0).x)())"`)
    })

    it('detaches the receiver through parentheses (audit round 2)', () => {
      const output = transpile('(flag)(); (flag)`t`;', { define: { flag: 'obj.method' } })
      expect(output).toContain('(0, obj.method)();')
      expect(output).toContain('(0, obj.method)`t`;')
      expect(output).toMatchInlineSnapshot(`"(0, obj.method)(); (0, obj.method)\`t\`;"`)
    })

    it('parenthesizes a member-path string standing as a statement (audit round 2)', () => {
      const output = transpile('a.b;\nlog(x)', {
        define: { 'a.b': '"use strict"' },
      })
      expect(output).toContain('("use strict");')
      expect(output).toMatchInlineSnapshot(`
        "("use strict");
        log(x)"
      `)
    })

    it('substitutes only identifier-shaped values in JSX tags (audit round 6)', () => {
      const output = transpile('<><FLAG /><FLAG n={FLAG} /></>', {
        define: { FLAG: 'Comp.Box' },
        lang: 'tsx' as never,
      })
      expect(output).toContain('<Comp.Box /><Comp.Box n={Comp.Box} />')
      for (const value of ['"x"', 'true', 'undefined', '-1']) {
        // petrea keeps JSX text: a literal tag would be invalid JSX or the
        // wrong element type, so the reference stays
        expect(transpile('<FLAG />', { define: { FLAG: value }, lang: 'tsx' as never })).toContain('<FLAG />')
      }
    })

    it('guards JSX tag case and member-tag roots (audit round 7)', () => {
      // a bare lowercase splice would flip the component to an intrinsic
      // string tag; esbuild passes the variable because it lowers tags to
      // createElement arguments
      expect(transpile('<FLAG />', { define: { FLAG: 'component' }, lang: 'tsx' as never })).toContain('<FLAG />')
      expect(transpile('<FLAG />', { define: { FLAG: 'Component' }, lang: 'tsx' as never })).toContain('<Component />')
      expect(transpile('<FLAG />', { define: { FLAG: 'Comp.Box' }, lang: 'tsx' as never })).toContain('<Comp.Box />')
      expect(transpile('<FLAG />', { define: { FLAG: '_C' }, lang: 'tsx' as never })).toContain('<_C />')
      // member-tag roots are references regardless of case; literals stay out
      expect(transpile('<FLAG.X />', { define: { FLAG: 'component' }, lang: 'tsx' as never })).toContain('<component.X />')
      expect(transpile('<FLAG.X />', { define: { FLAG: '"x"' }, lang: 'tsx' as never })).toContain('<FLAG.X />')
      expect(transpile('<FLAG.X />', { define: { FLAG: 'undefined' }, lang: 'tsx' as never })).toContain('<FLAG.X />')
      expect(transpile('<FLAG.X />', { define: { FLAG: 'Comp' }, lang: 'tsx' as never })).toContain('<Comp.X />')
    })

    it('keeps optional-call short-circuit for unary callees (audit round 8)', () => {
      const output = transpile('var calls = 0; try { FLAG?.(++calls) } catch {} ', {
        define: { FLAG: 'undefined' },
      })
      expect(output).toContain('(void 0)?.(++calls)')
      // the optional call short-circuits: the argument never runs
      const calls = new Function(`var calls = 0; try { ${output} } catch {} return calls`)()
      expect(calls).toBe(0)
      expect(output).toMatchInlineSnapshot(`"var calls = 0; try { (void 0)?.(++calls) } catch {} "`)
    })

    it('keeps escaped spellings out of JSX tags (audit round 8)', () => {
      const warnings: string[] = []
      const output = transpile('<FLAG />', {
        define: { FLAG: '\\u0043omp' },
        lang: 'tsx' as never,
        onWarn: warning => warnings.push(warning.type),
      })
      expect(output).toContain('<FLAG />')
      expect(warnings).toEqual(['define-jsx-tag'])
    })

    it('warns when a JSX tag substitution is skipped (audit round 8)', () => {
      const warnings: Array<{ type: string, start: number, end: number }> = []
      const output = transpile('const a = <FLAG />; const b = <FLAG n={FLAG} />', {
        define: { FLAG: 'component' },
        lang: 'tsx' as never,
        onWarn: warning => warnings.push(warning),
      })
      // only the tag position is skipped and warned; the attribute reads on
      expect(output).toContain('<FLAG />')
      expect(output).toContain('<FLAG n={component} />')
      expect(warnings).toEqual([
        { type: 'define-jsx-tag', start: 11, end: 15 },
        { type: 'define-jsx-tag', start: 31, end: 35 },
      ])
    })

    it('resolves accessor computed keys in the enclosing this (audit round 7)', () => {
      const output = transpile(
        [
          'class C { accessor [this.foo] = 1 }',
          'class D { accessor x = this.foo }',
          'function f() { return class { accessor [this.x] = 1 } }',
        ].join('\n'),
        { define: { 'this.foo': '"defined"', 'this.x': '"w"' } },
      )
      expect(output).toContain('accessor [\"defined\"] = 1')
      expect(output).toContain('accessor x = this.foo')
      expect(output).toContain('accessor [this.x] = 1')
      expect(output).toMatchInlineSnapshot(`
        "class C { accessor ["defined"] = 1 }
        class D { accessor x = this.foo }
        function f() { return class { accessor [this.x] = 1 } }"
      `)
    })

    it('replaces destructuring defaults, which are read positions (audit round 6)', () => {
      const output = transpile(['[x=FLAG]=[]', '({x=FLAG}={})', '({x:y=FLAG}={})'].join('\n'), {
        define: { FLAG: '1' },
      })
      expect(output).toContain('[x=1]=[]')
      expect(output).toContain('({x=1}={})')
      expect(output).toContain('({x:y=1}={})')
      expect(output).toMatchInlineSnapshot(`
        "[x=1]=[]
        ({x=1}={})
        ({x:y=1}={})"
      `)
    })

    it('treats auto-accessor initializers as instance this (audit round 6)', () => {
      const output = transpile('class C { accessor x = this.foo }', {
        define: { 'this.foo': '1' },
      })
      expect(output).toContain('this.foo')
      expect(output).toMatchInlineSnapshot(`"class C { accessor x = this.foo }"`)
    })

    it('unwraps transparent wrappers in computed keys (audit round 6)', () => {
      const output = transpile('log(a[("b")]); log(a["b" as string]); log(a.b)', {
        define: { 'a.b': '1' },
      })
      expect(output).toContain('log(1); log(1); log(1)')
      expect(output).toMatchInlineSnapshot(`"log(1); log(1); log(1)"`)
    })

    it('lets this and import.meta through a with body (audit round 6)', () => {
      const output = transpile(
        'with(obj){result=this.x; flag=FLAG; meta=import.meta.env}',
        { define: { 'this.x': '1', 'FLAG': '1', 'import.meta.env': '1' } },
      )
      expect(output).toContain('result=1')
      expect(output).toContain('flag=FLAG')
      expect(output).toContain('meta=1')
      expect(output).toMatchInlineSnapshot(`"with(obj){result=1; flag=FLAG; meta=1}"`)
    })

    it('guards write targets behind TS wrappers (audit round 5)', () => {
      const output = transpile(['FLAG! = 2', 'FLAG!++'].join('\n'), {
        define: { FLAG: '1' },
      })
      expect(output).toContain('FLAG  = 2')
      expect(output).toContain('FLAG ++')
      expect(output).not.toMatch(/\b1\b/)
      expect(output).toMatchInlineSnapshot(`
        "FLAG  = 2
        FLAG ++"
      `)
    })

    it('guards a parenthesized as-expression write target (audit round 5)', () => {
      // parses through recovery on its own; the write guard keeps the
      // reference so the splice cannot emit `(1) = 2`
      const output = transpile('(FLAG as any) = 2', { define: { FLAG: '1' } })
      expect(output).toContain('FLAG')
      expect(output).not.toMatch(/\b1\b/)
      expect(output).toMatchInlineSnapshot(`"(FLAG       ) = 2"`)
    })

    it('keeps a deleted bare identifier, still replacing member deletes (audit round 5)', () => {
      const output = transpile(['delete FLAG', 'delete process.env.NODE_ENV'].join('\n'), {
        define: { 'FLAG': '1', 'process.env.NODE_ENV': '"x"' },
      })
      expect(output).toContain('delete FLAG')
      expect(output).toContain('delete \"x\"')
      expect(output).toMatchInlineSnapshot(`
        "delete FLAG
        delete "x""
      `)
    })

    it('unwraps nested transparent wrappers in member chains (audit round 5)', () => {
      const output = transpile('log(((a)).b); log(((a.b)).c)', {
        define: { 'a.b': '1', 'a.b.c': '2' },
      })
      expect(output).toContain('log(1); log(2)')
      expect(output).toMatchInlineSnapshot(`"log(1); log(2)"`)
    })

    it('resolves computed-key this against the enclosing function (audit round 5)', () => {
      const output = transpile(
        ['function f() { return class { [this.x] = 1 } }', 'class C { [this.x] = 1 }'].join('\n'),
        { define: { 'this.x': '"top"' } },
      )
      // inside f the key reads f's `this`; at top level it is the top `this`
      expect(output).toContain('return class { [this.x] = 1 }')
      expect(output).toContain('class C { [\"top\"] = 1 }')
      expect(output).toMatchInlineSnapshot(`
        "function f() { return class { [this.x] = 1 } }
        class C { ["top"] = 1 }"
      `)
    })

    it('guards unary splices in ** left and new callee (audit round 4)', () => {
      const output = transpile(['FLAG ** 2', '2 ** FLAG', 'new FLAG()'].join('\n'), {
        define: { FLAG: 'undefined' },
      })
      expect(output).toContain('(void 0) ** 2')
      expect(output).toContain('2 ** void 0')
      expect(output).toContain('new (void 0)()')
      expect(output).toMatchInlineSnapshot(`
        "(void 0) ** 2
        2 ** void 0
        new (void 0)()"
      `)

      const negative = transpile('new FLAG()', { define: { FLAG: '-1' } })
      expect(negative).toContain('new (-1)()')
      expect(negative).toMatchInlineSnapshot(`"new (-1)()"`)
    })

    it('sees precedence and directives through TS wrappers (audit round 4)', () => {
      const output = transpile(['FLAG!.x', 'FLAG! ** 2'].join('\n'), {
        define: { FLAG: 'undefined' },
      })
      expect(output).toContain('(void 0) .x')
      expect(output).toContain('** 2')
      expect(output).toMatchInlineSnapshot(`
        "(void 0) .x
        (void 0)  ** 2"
      `)

      const negated = transpile('FLAG! ** 2', { define: { FLAG: '-1' } })
      expect(negated).toContain('(-1)')
      expect(negated).toMatchInlineSnapshot(`"(-1)  ** 2"`)

      const directive = transpile('FLAG as any;\nwith(x) {}', {
        define: { FLAG: '"use strict"' },
      })
      expect(directive).toContain('("use strict")')
      expect(directive).toMatchInlineSnapshot(`
        "("use strict")       ;
        with(x) {}"
      `)
    })

    it('detaches through generic instantiation expressions (audit round 4)', () => {
      const output = transpile('(FLAG<number>)()', { define: { FLAG: 'obj.method' } })
      expect(output).toContain('(0, obj.method)')
      expect(output).toMatchInlineSnapshot(`"((0, obj.method)        )()"`)
    })

    it('never replaces inside a with body, only its object (audit round 4)', () => {
      const output = transpile(
        'with(obj){return FLAG}\nwith(obj){return FLAG.x}\nwith(FLAG){}',
        { define: { FLAG: '1' } },
      )
      expect(output).toContain('return FLAG}')
      expect(output).toContain('return FLAG.x}')
      expect(output).toContain('with(1){}')
      expect(output).toMatchInlineSnapshot(`
        "with(obj){return FLAG}
        with(obj){return FLAG.x}
        with(1){}"
      `)
    })

    it('drops accepted trailing trivia after an undefined chain (audit round 4)', () => {
      const output = transpile('log(FLAG); after()', { define: { FLAG: 'undefined.x //c' } })
      expect(output).toBe('log((void 0).x); after()')
      expect(output).not.toContain('//c')
    })

    it('matches member chains through transparent wrappers (audit round 4)', () => {
      const output = transpile('log((a).b); log(a!.b)', { define: { 'a.b': '1' } })
      expect(output).toContain('log(1); log(1)')
      expect(output).toMatchInlineSnapshot(`"log(1); log(1)"`)
    })

    it('parenthesizes by expression position, not adjacency (audit round 3)', () => {
      // member objects in every spelling: `FLAG["x"]` and `FLAG .x` are
      // member accesses regardless of what byte follows the reference
      const output = transpile(
        [
          'log(FLAG["x"])',
          'log(FLAG .x)',
          'log(FLAG?.x)',
          'log((FLAG).x)',
          'log(FLAG)',
        ].join('\n'),
        { define: { FLAG: 'undefined' } },
      )
      expect(output).toContain('log((void 0)["x"])')
      expect(output).toContain('log((void 0) .x)')
      expect(output).toContain('log((void 0)?.x)')
      expect(output).toContain('log((void 0).x)')
      expect(output).toContain('log(void 0)')
      expect(output).toMatchInlineSnapshot(`
        "log((void 0)["x"])
        log((void 0) .x)
        log((void 0)?.x)
        log((void 0).x)
        log(void 0)"
      `)

      const numeric = transpile(
        'log(FLAG["toString"]()); log(FLAG .toString()); log(FLAG.x); log(2 ** FLAG)',
        { define: { FLAG: '-1' } },
      )
      expect(numeric).toContain('log((-1)["toString"]())')
      expect(numeric).toContain('log((-1) .toString())')
      expect(numeric).toContain('log((-1).x)')
      expect(numeric).toContain('log(2 ** -1)')
      expect(numeric).toMatchInlineSnapshot(`"log((-1)["toString"]()); log((-1) .toString()); log((-1).x); log(2 ** -1)"`)
    })

    it('detaches through transparent TS wrappers (audit round 3)', () => {
      const output = transpile('(flag as any)(); (flag!)(); flag();', {
        define: { flag: 'obj.method' },
      })
      expect(output).toContain('(0, obj.method)();')
      // through a wrapper the splice keeps the reference's own span, so the
      // erased wrapper stays around the detached text — valid, receiver off
      expect(output).toContain('((0, obj.method)       )();')
      expect(output).toMatchInlineSnapshot(`"((0, obj.method)       )(); ((0, obj.method) )(); (0, obj.method)();"`)
    })

    it('rewrites escaped undefined-rooted chains by root span (audit round 3)', () => {
      const output = transpile('log(FLAG)', { define: { FLAG: '\\u0075ndefined.x' } })
      expect(output).toContain('log((void 0).x)')
      expect(output).toMatchInlineSnapshot(`"log((void 0).x)"`)
    })

    it('accepts negative numbers with position-aware parentheses (audit round 2)', () => {
      const output = transpile(
        [
          'log(FLAG)',
          'log(FLAG * 2)',
          'const a = 2 ** FLAG',
          'const b = FLAG ** 2',
          'const c = x-FLAG',
          'const d = -FLAG',
          'log(FLAG.x)',
          'const e = FLAG.toFixed(2)',
        ].join('\n'),
        { define: { FLAG: '-1' } },
      )
      expect(output).toContain('log(-1)')
      expect(output).toContain('log(-1 * 2)')
      expect(output).toContain('2 ** -1')
      expect(output).toContain('(-1) ** 2')
      expect(output).toContain('x-(-1)')
      expect(output).toContain('-(-1)')
      expect(output).toContain('log((-1).x)')
      expect(output).toContain('(-1).toFixed(2)')
      expect(output).toMatchInlineSnapshot(`
        "log(-1)
        log(-1 * 2)
        const a = 2 ** -1
        const b = (-1) ** 2
        const c = x-(-1)
        const d = -(-1)
        log((-1).x)
        const e = (-1).toFixed(2)"
      `)
    })

    it('reads all value shapes and guards their writes (TestDefineAssignWarning)', () => {
      const define = {
        'a': 'null',
        'b.c': 'null',
        'd': 'ident',
        'e.f': 'ident',
        'g': 'dot.chain',
        'h.i': 'dot.chain',
      }
      const read = transpile(
        'console.log([a, b.c, b["c"]], [d, e.f, e["f"]], [g, h.i, h["i"]])',
        { define },
      )
      expect(read).toContain('[null, null, null]')
      expect(read).toContain('[ident, ident, ident]')
      expect(read).toContain('[dot.chain, dot.chain, dot.chain]')
      expect(read).toMatchInlineSnapshot(`"console.log([null, null, null], [ident, ident, ident], [dot.chain, dot.chain, dot.chain])"`)

      const write = transpile(
        'console.log([a = 0, b.c = 0, b["c"] = 0], [d = 0, e.f = 0, e["f"] = 0], [g = 0, h.i = 0, h["i"] = 0])',
        { define },
      )
      expect(write).toContain('[a = 0, b.c = 0, b["c"] = 0]')
      expect(write).toContain('[ident = 0, ident = 0, ident = 0]')
      expect(write).toContain('[dot.chain = 0, dot.chain = 0, dot.chain = 0]')
      expect(write).toMatchInlineSnapshot(`"console.log([a = 0, b.c = 0, b["c"] = 0], [ident = 0, ident = 0, ident = 0], [dot.chain = 0, dot.chain = 0, dot.chain = 0])"`)
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
