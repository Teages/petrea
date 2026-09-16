import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

// rollup/rolldown-verified plain-text replacement: verbatim splice, identifier
// boundaries, hits inside strings/comments/templates included.
describe('replace', () => {
  describe('basics', () => {
    it('replaces a bare key with an exact splice', () => {
      const output = transpile('log(FLAG)', { replace: { FLAG: '1' } })
      expect(output).toBe('log(1)')
      expect(output).not.toContain('FLAG')
      expect(output.length).toBe('log(FLAG)'.length - 3)
    })

    it('does not match part of a longer identifier', () => {
      const input = 'log(FLAGX)\nlog(FLAGX2)'
      expect(transpile(input, { replace: { FLAG: '1' } })).toBe(input)
    })

    it('does not match when followed by a dot', () => {
      const input = 'log(FLAG.x)'
      expect(transpile(input, { replace: { FLAG: '1' } })).toBe(input)
    })

    it('prefers the longest matching key', () => {
      const output = transpile('log(a.b)', { replace: { 'a': '1', 'a.b': '22' } })
      expect(output).toBe('log(22)')
    })

    it('splices every kind of hit — literal, comment, substitution — alike', () => {
      const input = [
        'const s = "FLAG";',
        '/* FLAG */',
        'const t = `${FLAG}`;',
        'const u = `FLAG`;',
      ].join('\n')
      const output = transpile(input, { replace: { FLAG: '1' } })
      expect(output).toMatchInlineSnapshot(`
        "const s = "1";
        /* 1 */
        const t = \`\${1}\`;
        const u = \`1\`;"
      `)
    })

    it('splices a hit crossing into a string literal exactly too', () => {
      const output = transpile('f("FLAG");', { replace: { 'f("FLAG': 'f("X' } })
      expect(output).toBe('f("X");')
      expect(output.length).toBe('f("FLAG");'.length - 3)
    })

    it('splices a hit stopping at a quote exactly, before the literal', () => {
      expect(transpile('log("FLAG")', { replace: { 'log(': 'f(' } })).toBe('f("FLAG")')
    })

    it('splices quote characters a value carries verbatim (no context logic)', () => {
      const output = transpile('log(START + FLAG + END)', {
        replace: { START: '"', FLAG: 'x', END: '"' },
      })
      expect(output).toBe('log(" + x + ")')
    })

    it('splices inside JSXText verbatim under tsx', () => {
      expect(transpile('<div>FLAG</div>', { lang: 'tsx', replace: { FLAG: 'x' } })).toBe(
        '<div>x</div>',
      )
    })

    it('replaces with an empty value', () => {
      const output = transpile('log(FLAG)', { replace: { FLAG: '' } })
      expect(output).toBe('log()')
    })

    it('accepts number values as their string spelling', () => {
      const output = transpile('log(FLAG)', { replace: { FLAG: 42 } })
      expect(output).toBe('log(42)')
    })

    it('lets a longer value shift the line', () => {
      const output = transpile('log(FLAG)', { replace: { FLAG: 'production' } })
      expect(output).toBe('log(production)')
      expect(output.length).toBeGreaterThan('log(FLAG)'.length)
    })

    it('keeps a user-padded value tail verbatim (the DIY column guarantee)', () => {
      // trailing spaces are part of the value and splice verbatim
      const output = transpile('log(FLAG);', { replace: { FLAG: '1   ' } })
      expect(output).toBe('log(1   );')
      expect(output.length).toBe('log(FLAG);'.length)
    })

    it('matches at the identifier boundary of any unicode neighborhood', () => {
      // é, λ and 变 are all >= \xA0: inside the boundary class, so the key
      // reads as part of a longer word
      expect(transpile('éFLAG', { replace: { FLAG: '1' } })).toBe('éFLAG')
      expect(transpile('λFLAG', { replace: { FLAG: '1' } })).toBe('λFLAG')
      expect(transpile('变FLAG', { replace: { FLAG: '1' } })).toBe('变FLAG')
      expect(transpile(';FLAG', { replace: { FLAG: '1' } })).toBe(';1')
    })

    it('matches at the start and end of the file and after a newline', () => {
      expect(transpile('FLAG', { replace: { FLAG: '1' } })).toBe('1')
      expect(transpile('x;\nFLAG', { replace: { FLAG: '1' } })).toBe('x;\n1')
    })

    it('keeps scanning after a match (leftmost-longest, non-overlapping)', () => {
      const output = transpile('aa + aa', { replace: { aa: '1', a: '2' } })
      expect(output).toBe('1 + 1')
    })
  })
})
