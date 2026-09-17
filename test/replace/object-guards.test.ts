import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

// `objectGuards` derives `typeof prefix` guard keys from every legal
// member-chain key, valued as the quoted literal `"object"`.
describe('replace', () => {
  describe('objectGuards', () => {
    const options = {
      replace: { 'window.document': 'W' },
      replaceOptions: { objectGuards: true },
    }

    it('replaces the typeof guard with the quoted literal "object"', () => {
      const output = transpile('log(typeof window)', options)
      expect(output).toBe('log("object")')
    })

    it('derives every proper dotted prefix', () => {
      const guards = {
        replace: { 'a.b.c': 'X' },
        replaceOptions: { objectGuards: true },
      }
      expect(transpile('typeof a', guards)).toBe('"object"')
      expect(transpile('typeof a.b', guards)).toBe('"object"')
    })

    it('derives guards for chains spelled with ID_Continue characters', () => {
      // a combining accent and the zero-width joiners continue an identifier
      // (ID_Continue), judged with the parser's own tables
      const accent = {
        replace: { 'e\u0301.x': 'X' },
        replaceOptions: { objectGuards: true },
      }
      expect(transpile('log(typeof e\u0301)', accent)).toBe('log("object")')
      expect(transpile('log(e\u0301.x)', accent)).toBe('log(X)')
      const zwnj = {
        replace: { 'a\u200Cb.c': 'Y' },
        replaceOptions: { objectGuards: true },
      }
      expect(transpile('log(typeof a\u200Cb)', zwnj)).toBe('log("object")')
      const zwj = {
        replace: { 'x\u200Dy.z': 'Z' },
        replaceOptions: { objectGuards: true },
      }
      expect(transpile('log(typeof x\u200Dy)', zwj)).toBe('log("object")')
    })

    it('dedupes guards across keys', () => {
      const table = {
        replace: { 'process.env.A': '1', 'process.env.B': '2' },
        replaceOptions: { objectGuards: true },
      }
      expect(transpile('typeof process.env', table)).toBe('"object"')
      expect(transpile('typeof process', table)).toBe('"object"')
      expect(transpile('log(process.env.A)', table)).toBe('log(1)')
    })

    it('emits a quoted literal so the guard comparison stays constant', () => {
      // a constant comparison instead of a ReferenceError on an undeclared
      // `object` identifier
      const output = transpile('if (typeof window !== "undefined") { f() }', options)
      expect(output).toBe('if ("object" !== "undefined") { f() }')
    })

    it('does not hit the guard inside a longer chain (the dot rule)', () => {
      const output = transpile('typeof window.document', options)
      expect(output).toBe('typeof W')
    })

    it('matches guard text literally (double space misses)', () => {
      expect(transpile('typeof  window', options)).toBe('typeof  window')
    })

    it('gives user keys priority over derived guards', () => {
      const output = transpile('typeof window', {
        replace: { 'window.document': 'W', 'typeof window': 'T' },
        replaceOptions: { objectGuards: true },
      })
      expect(output).toBe('T')
    })

    it('derives a guard even when a shorter user key exists (conflict reads the full key)', () => {
      // `a` is a user key but `typeof a` is not: the `a.b` guard must still
      // derive
      const conflict = {
        replace: { 'a': 'X', 'a.b': 'Y' },
        replaceOptions: { objectGuards: true },
      }
      expect(transpile('log(typeof a);', conflict)).toBe('log("object");')
      expect(transpile('a; a.b;', conflict)).toBe('X; Y;')
    })

    it('derives nothing when off (default)', () => {
      expect(transpile('log(typeof window)', { replace: { 'window.document': 'W' } })).toBe(
        'log(typeof window)',
      )
    })
  })
})
