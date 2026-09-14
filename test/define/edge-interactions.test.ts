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
  describe('edge interactions', () => {
    it('parenthesizes async in a member-chain for-of target', () => {
      const output = transpile(
        'for (obj.FLAG of [1]) {}\nlog(obj.FLAG)',
        { define: { 'obj.FLAG': 'async' } },
      )
      expect(output).toContain('for ((async) of [1])')
      expect(output).toContain('log(async)')
    })

    it('keeps a function-enclosed class decorator reading that function this', () => {
      const output = transpile(
        [
          'function dec(x) { return x }',
          'function f() { return class { @dec(this.x) field } }',
          'class Top { @dec(this.y) field }',
        ].join('\n'),
        { define: { 'this.x': '1', 'this.y': '2' } },
      )
      // the decorator evaluates in the scope enclosing the member: inside
      // f that is f's this, so it stays; at the top level it replaces
      expect(output).toContain('@dec(this.x)')
      expect(output).toContain('@dec(2)')
    })

    it('wraps every non-chain decorator head shape in parentheses', () => {
      const shapes: Array<[string, string]> = [
        ['42', '@(42)'],
        ['"x"', '@("x")'],
        ['true', '@(true)'],
        ['null', '@(null)'],
        ['this', '@(this)'],
        ['import.meta', '@(import.meta)'],
        ['undefined', '@(void 0)'],
        ['-1', '@(-1)'],
      ]
      for (const [value, expected] of shapes) {
        const output = transpile('@FLAG class C {}', { define: { FLAG: value } })
        expect(output).toContain(expected)
      }
      // a dotted identifier chain and a bare name stay unparenthesized
      expect(transpile('@FLAG class C {}', { define: { FLAG: 'obj.method' } })).toContain('@obj.method')
      expect(transpile('@FLAG class C {}', { define: { FLAG: 'DEBUG' } })).toContain('@DEBUG')
    })

    it('wraps the whole decorator head when the call joins the splice', () => {
      // the paren form allows nothing after it, so the call must go inside
      const output = transpile('@FLAG() class C {}', { define: { FLAG: '42' } })
      expect(output).toContain('@(42())')
      // a member chain with its call stays bare
      expect(transpile('@FLAG() class C {}', { define: { FLAG: 'obj.method' } })).toContain('@obj.method()')
      // a member suffix above the splice joins the wrap, spaced like a
      // numeric member access
      expect(transpile('@FLAG.x class C {}', { define: { FLAG: '42' } })).toContain('@(42 .x)')
      // argument positions are ordinary expressions
      expect(transpile('@dec(FLAG) class C {}', { define: { FLAG: '42' } })).toContain('@dec(42)')
    })

    it('detaches the receiver of an enum-qualified call splice', () => {
      const output = transpile(
        [
          'function makeFn() { return function () { return this === undefined ? 1 : 2 } }',
          'enum E { Fn = makeFn(), X = FLAG() }',
          'log(E.X)',
        ].join('\n'),
        { define: { FLAG: 'Fn' } },
      )
      // the qualification turns a bare name into a chain: the original
      // call had no receiver, the splice must not gain one
      expect(output).toContain('(0, E.Fn)()')
      expect(output).not.toContain('E.Fn()')
    })

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
