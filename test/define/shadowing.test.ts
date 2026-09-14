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
  describe('shadowing', () => {
    it('does not shadow through erased ambient declarations', () => {
      const define = { FLAG: '1', FLAG2: '1', FLAG3: '1', FLAG4: '1' } as Record<string, string>
      const output = transpile(
        [
          'declare class FLAG {}',
          'declare enum FLAG2 { A }',
          'import type { FLAG3 } from "unused"',
          'import { type FLAG4, kept } from "unused"',
          'log(FLAG, FLAG2, FLAG3, FLAG4)',
        ].join('\n'),
        { define },
      )
      expect(output).toContain('log(1, 1, 1, 1)')
      expect(output).toContain('kept')
    })

    it('keeps namespace var inside the namespace scope', () => {
      const output = transpile(
        [
          'declare namespace N { var FLAG: number }',
          'namespace M { var FLAG = 1; export function get() { return FLAG } }',
          'log(FLAG)',
        ].join('\n'),
        { define: { FLAG: '2' } },
      )
      expect(output).toContain('log(2)')
      expect(output).toContain('return FLAG')
    })

    it('honors import-equals bindings, but not type-only ones', () => {
      const output = transpile(
        [
          'const ns = { foo: 2 }',
          'import FLAG = ns.foo',
          'import type TFLAG = require("m")',
          'log(FLAG, TFLAG)',
        ].join('\n'),
        { define: { FLAG: '1', TFLAG: '3' } },
      )
      expect(output).toContain('log(FLAG, 3)')
      expect(output).toContain('import FLAG = ns.foo')
    })

    it('expands the __proto__ shorthand through a computed key', () => {
      // fromEntries creates an own property, unlike the `__proto__:` setter
      const define = Object.fromEntries([['__proto__', 'null']])
      const output = transpile(
        'const o = {__proto__}\nconst p = { other: __proto__ }',
        { define },
      )
      // the plain `__proto__:` spelling would set the prototype instead of
      // defining the own property the shorthand reads
      expect(output).toContain('{["__proto__"]: null}')
      expect(output).toContain('{ other: null }')
    })

    it('parenthesizes async in a for-of write target', () => {
      const output = transpile(
        'for (FLAG of [1]) {}\nlog(FLAG)',
        { define: { FLAG: 'async' } },
      )
      expect(output).toContain('for ((async) of [1])')
      expect(output).toContain('log(async)')
    })

    it('parenthesizes unary splices in class heritage and decorators', () => {
      const output = transpile(
        'class A extends FLAG {}\n@FLAG class B {}\nclass C extends FLAG.x {}',
        { define: { FLAG: 'undefined' } },
      )
      expect(output).toContain('extends (void 0)')
      // the heritage arm must not fire for the member's object position
      expect(output).toContain('extends (void 0).x')
      expect(output).not.toContain('extends void 0')
    })

    it('parenthesizes negative numeric decorator values', () => {
      const output = transpile('@FLAG class C {}', { define: { FLAG: '-1' } })
      expect(output).toContain('@(-1)')
      expect(output).not.toContain('@-1')
    })

    it('treats decorator this as the enclosing this, not the instance', () => {
      const output = transpile(
        [
          'class C {',
          '  @dec(this.x) field',
          '  @dec(this.y) accessor a',
          '  @dec(this.z) method() {}',
          '  inner = this.w',
          '}',
        ].join('\n'),
        { define: { 'this.x': '1', 'this.y': '2', 'this.z': '3' } },
      )
      expect(output).toContain('@dec(1)')
      expect(output).toContain('@dec(2)')
      expect(output).toContain('@dec(3)')
      expect(output).toContain('this.w')
    })

    it('skips arguments inside functions, an implicit binding no registry sees', () => {
      const define = { 'arguments.length': '0', 'arguments': 'null' } as Record<string, string>
      const output = transpile(
        [
          'function f() { return arguments.length; const g = () => arguments }',
          'class K { m() { return arguments.length } }',
          'log(arguments.length)',
        ].join('\n'),
        { define },
      )
      expect(output).toContain('return arguments.length')
      expect(output).toContain('() => arguments')
      expect(output).toContain('log(0)')
      expect(output).not.toContain('log(arguments.length)')
    })

    it('replaces arguments at module top level, where no binding exists', () => {
      const output = transpile(
        'const g = () => arguments.length\nlog(arguments)',
        { define: { 'arguments.length': '0', 'arguments': 'null' } },
      )
      expect(output).toContain('() => 0')
      expect(output).toContain('log(null)')
    })

    it('parenthesizes a unary splice as a private field object', () => {
      const output = transpile(
        'class K { #x = 1\n  m() { return FLAG?.#x }\n  n() { return FLAG.#x } }\nlog(FLAG?.#y)',
        { define: { FLAG: 'undefined' } },
      )
      // `void 0?.#x` would dereference the number instead of short-circuiting
      expect(output).toContain('(void 0)?.#x')
      expect(output).toContain('(void 0).#x')
      expect(output).toContain('(void 0)?.#y')
      expect(output).not.toContain('void 0?')
    })

    it('separates a numeric splice from a following private field', () => {
      const output = transpile(
        'class K { #x = 1\n  m() { return FLAG.#x } }',
        { define: { FLAG: '42' } },
      )
      expect(output).toContain('42 .#x')
      expect(output).not.toContain('42.#x')
    })

    it('skips references a parameter shadows on an enum-declaring file', () => {
      // enum files forgo the bound-name fast path (member scopes are
      // invisible to the binding scan), so their shadow checks must still
      // run against the precise model — the gate's bound list triggers it
      const output = transpile(
        [
          'enum E { A }',
          'function f(FLAG) { return FLAG }',
          'f(7)',
        ].join('\n'),
        { define: { FLAG: '1' } },
      )
      expect(output).toContain('function f(FLAG) { return FLAG }')
      expect(output).not.toContain('return 1')
    })

    it('skips references a parameter shadows with only an empty ambient enum', () => {
      const output = transpile(
        [
          'declare enum E1 {}',
          'function f(FLAG) { return FLAG }',
          'f(7)',
        ].join('\n'),
        { define: { FLAG: '1' } },
      )
      expect(output).toContain('function f(FLAG) { return FLAG }')
    })

    it('qualifies member references when a define key matches a member name', () => {
      const output = transpile(
        'enum E { A, B = f(A) }\nlog(A)',
        { define: { A: '1' } },
      )
      expect(output).toContain('f(E.A)')
      expect(output).toContain('log(1)')
      expect(output).not.toContain('f(1)')
    })

    it('qualifies computed-string member references under a matching key', () => {
      const output = transpile(
        'enum E { ["FLAG"], B = g(FLAG) }',
        { define: { FLAG: '1' } },
      )
      expect(output).toContain('g(E.FLAG)')
      expect(output).not.toContain('g(1)')
    })

    it('still replaces global references on enum files whose members do not collide', () => {
      const output = transpile(
        'enum E { A }\nlog(FLAG)',
        { define: { FLAG: '1' } },
      )
      expect(output).toContain('log(1)')
      expect(output).not.toContain('log(FLAG)')
    })

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
})
