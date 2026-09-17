import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

// `replace` is a verbatim pre-parse splice, so the output length changes by
// the sum of (value length − matched length) over all hits, in UTF-16 units.
describe('replace', () => {
  describe('invariants', () => {
    it('preserves a line exactly when the splice is length-equal', () => {
      const input = ['const a = FLAG;', 'const b = 2;', 'log(FLAG)'].join('\n')
      const output = transpile(input, { replace: { FLAG: 'abcd' } })
      expect(output.length).toBe(input.length)
      expect(output.split('\n')).toHaveLength(input.split('\n').length)
      for (const [index, line] of input.split('\n').entries()) {
        expect(output.split('\n')[index], `line ${index + 1}`).toHaveLength(line.length)
      }
      expect(output).toMatchInlineSnapshot(`
        "const a = abcd;
        const b = 2;
        log(abcd)"
      `)
    })

    it('changes length by the exact splice arithmetic (UTF-16 units)', () => {
      const input = 'const s = "FLAG";\nlog(FLAG);'
      const output = transpile(input, { replace: { FLAG: '1' } })
      expect(output.length).toBe(input.length - 6)
      expect(output.split('\n')[0]).toBe('const s = "1";')
      expect(output.split('\n')[1]).toBe('log(1);')

      // λ is 2 UTF-8 bytes but 1 UTF-16 unit: the JS-visible length moves
      // by the unit difference
      const lambda = transpile('log(FLAG)', { replace: { FLAG: 'λ' } })
      expect(lambda).toBe('log(λ)')
      expect(lambda.length).toBe('log(FLAG)'.length - 3)

      const longer = transpile('log(FLAG)', { replace: { FLAG: 'longer' } })
      expect(longer).toBe('log(longer)')
      expect(longer.length).toBe('log(FLAG)'.length + 2)
    })

    it('is off by default and accepts an empty map', () => {
      const input = 'const x = FLAG'
      expect(transpile(input)).toBe(input)
      expect(transpile(input, {})).toBe(input)
      expect(transpile(input, { replace: {} })).toBe(input)
    })

    it('round-trips a raw lone surrogate on the UTF-16 path', () => {
      const input = '// \uD800\nlog(FLAG)'
      const output = transpile(input, { replace: { FLAG: '1' } })
      expect(output.length).toBe(input.length - 3)
      expect(output).toContain(String.fromCharCode(0xD800))
      expect(output.endsWith('log(1)')).toBe(true)
    })

    it('scans the original units, so a U+FFFD key cannot match a lone surrogate', () => {
      const surrogate = String.fromCharCode(0xD800)
      // the lossy parse copy never participates in matching
      const phantom = `log("${surrogate}")`
      expect(transpile(phantom, { replace: { '\uFFFD': 'X' } })).toBe(phantom)
      // a genuine U+FFFD in the same file still replaces
      const genuine = `log("a${surrogate}b \uFFFD")`
      expect(transpile(genuine, { replace: { '\uFFFD': 'X' } })).toBe(
        `log("a${surrogate}b X")`,
      )
    })
  })
})
