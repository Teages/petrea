import { Buffer } from 'node:buffer'
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
  describe('length-preserving padding', () => {
    it('pads a short replacement so same-line columns hold', () => {
      // `process.env.NODE_ENV` is 20 chars, `"production"` is 12: the 8
      // trailing spaces keep `+ tail` at its original column (the ninth
      // space in the output is the input's own, outside the span)
      const input = 'const mode = process.env.NODE_ENV + tail'
      const output = transpile(input, { define: { 'process.env.NODE_ENV': '"production"' } })
      expect(output).toContain('"production"')
      expect(output.length).toBe(input.length)
      expect(output.indexOf('+ tail')).toBe(input.indexOf('+ tail'))
      expect(output).toMatchInlineSnapshot(`"const mode = "production"         + tail"`)
    })

    it('does not pad when the replacement is longer', () => {
      const output = transpile('log(a)', { define: { a: 'someRatherLongValue' } })
      expect(output).toBe('log(someRatherLongValue)')
    })

    it('pads in UTF-16 code units on the BOM-routed path', () => {
      // a leading BOM routes through the UTF-16 entry points; the padding
      // there counts code units, so the JS string lengths match exactly
      const input = '\uFEFFconst mode = process.env.NODE_ENV + tail'
      const output = transpile(input, { define: { 'process.env.NODE_ENV': '"production"' } })
      expect(output.length).toBe(input.length)
      expect(output.charCodeAt(0)).toBe(0xFEFF)
      expect(output.indexOf('+ tail')).toBe(input.indexOf('+ tail'))
    })

    it('pads on the lone-surrogate path without disturbing the surrogate', () => {
      // a raw lone surrogate also routes through the UTF-16 entry points
      const input = 'const s = "\uD800"; const mode = process.env.NODE_ENV + tail'
      const output = transpile(input, { define: { 'process.env.NODE_ENV': '"production"' } })
      expect(output.length).toBe(input.length)
      expect(output).toContain('\uD800')
      expect(output.indexOf('+ tail')).toBe(input.indexOf('+ tail'))
    })

    it('pads a multi-byte value in UTF-16 units on both paths', () => {
      // λ is 2 UTF-8 bytes but 1 UTF-16 unit: both the byte path and the
      // BOM-routed path pad to 4 units (`λ` + 3 spaces), keeping the JS
      // string positions — the byte path's output grows one byte in exchange
      const input = 'log(FLAG)'
      const bomInput = `\uFEFF${input}`
      const output = transpile(input, { define: { FLAG: 'λ' } })
      expect(output).toBe('log(λ   )')
      expect(output.length).toBe(input.length)
      expect(Buffer.byteLength(output, 'utf8')).toBe(Buffer.byteLength(input, 'utf8') + 1)
      const bomOutput = transpile(bomInput, { define: { FLAG: 'λ' } })
      expect(bomOutput).toBe(`\uFEFF${output}`)
      expect(bomOutput.length).toBe(bomInput.length)
    })

    it('pads a multi-byte span in UTF-16 units', () => {
      // `变量` is 6 UTF-8 bytes but 2 units: one space of padding, not five,
      // so the `)` keeps its column on the byte path
      const input = 'log(变量)'
      const output = transpile(input, { define: { 变量: '1' } })
      expect(output).toBe('log(1 )')
      expect(output.length).toBe(input.length)
      expect(output.indexOf(')')).toBe(input.indexOf(')'))
    })

    it('merges the numeric dot-separator with the padding', () => {
      // splice_text appends one space before a following member dot; the
      // padding counts it, so the run between `42` and `.x` is one merged
      // stretch of spaces and `.x` keeps its column
      const input = 'log(foo.NODE_ENV.x)'
      const output = transpile(input, { define: { 'foo.NODE_ENV': '42' } })
      expect(output).toMatch(/log\(42 +\.x\)/)
      expect(output).not.toContain('42.x')
      expect(output.length).toBe(input.length)
      expect(output.indexOf('.x')).toBe(input.indexOf('.x'))
      expect(output).toMatchInlineSnapshot(`"log(42          .x)"`)
    })

    it('pads an unwrapped decorator-head chain splice', () => {
      // `a.b` is a dotted identifier chain and stays bare at the head
      const input = '@DEBUG\nclass C {}'
      const output = transpile(input, { define: { DEBUG: 'a.b' } })
      expect(output.startsWith('@a.b')).toBe(true)
      expect(output.length).toBe(input.length)
      expect(output).toMatchInlineSnapshot(`
        "@a.b  
        class C {}"
      `)
    })

    it('leaves lengthening forms unpadded', () => {
      // shorthand expansion, receiver decoupling and directive wrapping all
      // make the text longer than the span: no padding applies
      expect(transpile('const o = { NODE_ENV }', { define: { NODE_ENV: '42' } }))
        .toMatchInlineSnapshot(`"const o = { NODE_ENV: 42 }"`)
      expect(transpile('cb()', { define: { cb: 'obj.method' } }))
        .toMatchInlineSnapshot(`";(0, obj.method)()"`)
      expect(transpile('SOME_FLAG', { define: { SOME_FLAG: '"x"' } }))
        .toMatchInlineSnapshot(`";("x")   "`)
    })

    it('pads the leading-semicolon splice at a statement head', () => {
      // `-1` at an expression-statement head takes the ASI-restoring `;`;
      // the padded splice keeps the following lines' columns
      const input = ['let a = 1', 'NEGFLAG', 'foo()'].join('\n')
      const output = transpile(input, { define: { NEGFLAG: '-1' } })
      expect(output.split('\n')[1]).toBe(';-1    ')
      expect(output.split('\n')[2]).toBe('foo()')
      expect(output.length).toBe(input.length)
    })

    it('keeps enum emission untouched by the padding', () => {
      // the enum pipeline's own text splices do not pad
      const output = transpile('enum E { A }', { define: { __DEV__: 'true' } })
      expect(output).toMatchInlineSnapshot(
        `"var  E; (function (E) { E[E["A"] = 0] = "A" })(E || (E = {}));"`,
      )
    })
  })
})
