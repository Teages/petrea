import { runInNewContext } from 'node:vm'
import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

/** Every non-newline code unit of a blanked region becomes a space. */
const spaces = (text: string) => text.replace(/[^\r\n]/g, ' ')

const dce = (input: string) => transpile(input, { dce: true })

describe('dce: statement boundaries', () => {
  it('keeps the if shell as an ASI barrier', () => {
    // a wholly blanked statement would let `a()` swallow the arrow across
    // the line break; the shell's braces keep the boundary by existing
    const input = 'a()\nif (false) { x() }\n(() => 2)()'
    const output = dce(input)
    expect(output).toBe('a()\nif (false) {     }\n(() => 2)()')
    expect(output.length).toBe(input.length)
  })

  it('keeps a labeled dead if attached to its label', () => {
    expect(dce('lbl: if (false) { a() }')).toBe('lbl: if (false) {     }')
  })

  it('hollows a dead if nested as a required body', () => {
    expect(dce('if (cond) if (false) { a() }')).toBe('if (cond) if (false) {     }')
    expect(dce('while (c) if (false) { a() }')).toBe('while (c) if (false) {     }')
  })

  it('never promotes a later string statement into a directive', () => {
    const input = [
      'function probe() {',
      '  "x";',
      '  if (false) {}',
      '  "use strict";',
      '  return this === undefined',
      '}',
      'const r = probe()',
    ].join('\n')
    const output = dce(input)
    // the hollowed if still separates the two strings, so "use strict"
    // stays an ordinary statement and sloppy-mode `this` survives —
    // removing the statement would activate strict mode instead
    const r = new Function(`${output}\nreturn r`)()
    expect(r).toBe(false)
  })

  it('preserves script completion values', () => {
    // the harness must see the difference: an empty statement in the
    // statement's place would complete to 1 (runInNewContext returns the
    // script's completion value; new Function would not)
    expect(runInNewContext('1; ;')).toBe(1)
    const cases: Array<[string, string]> = [
      ['1; if (false) { a() }', '1; if (false) {     }'],
      ['1; if (false) a()', '1; if (false) ;  '],
      ['1; while (false) a()', '1; while (false) ;  '],
      ['1; if (true) {} else { a() }', '1; if (true) {} else {     }'],
    ]
    for (const [input, expected] of cases) {
      const output = dce(input)
      expect(output, input).toBe(expected)
      expect(runInNewContext(output)).toBeUndefined()
    }
  })

  it('keeps comments outside the dead span and blanks those inside', () => {
    expect(dce('if (false) /*keep*/ a()\n//tail\nx()')).toBe(
      'if (false) /*keep*/ ;  \n//tail\nx()',
    )
    const input = 'if (false) { /*gone*/ a() }'
    expect(dce(input)).toBe(`if (false) {${spaces(' /*gone*/ a() ')}}`)
  })

  it('stays inert without the dce option', () => {
    expect(transpile('if (false) { a() }')).toBe('if (false) { a() }')
    expect(transpile('if (false) a()')).toBe('if (false) a()')
    expect(transpile('while (false) { a() }')).toBe('while (false) { a() }')
  })

  it('still fails loudly on parse errors with dce on', () => {
    expect(() => dce('if (false) { let x: = 1 }')).toThrow(SyntaxError)
  })
})

describe('dce: blanking fidelity', () => {
  it('keeps CRLF line breaks while hollowing', () => {
    const input = 'if (false) {\r\n  a()\r\n}'
    const output = dce(input)
    expect(output).toBe(`if (false) {\r\n${spaces('  a()')}\r\n}`)
    expect(output.length).toBe(input.length)
  })

  it('hollows through the UTF-16 entry (BOM input)', () => {
    // a leading BOM routes the input to the UTF-16 entry points
    const input = '\uFEFFif (false) { a() }'
    const output = dce(input)
    expect(output).toBe('\uFEFFif (false) {     }')
    expect(output.length).toBe(input.length)
  })

  it('keeps the positions of statements after an astral dead body', () => {
    const input = 'if (false) { const s = "🎁" }\nconst after = 1'
    const output = dce(input)
    expect(output.length).toBe(input.length)
    expect(output.endsWith('\nconst after = 1')).toBe(true)
    expect(output).not.toContain('🎁')
  })

  it('keeps every JavaScript line terminator inside a dead region', () => {
    // U+2028/U+2029 are line terminators too: the base blanker replaces them
    // (matching the ts-blank-space reference), but dce's edits reach plain
    // JavaScript no earlier pass rewrote, so it keeps all four
    const input = 'if (false) { a()\u2028 b() }\u2028const after = 1'
    const output = dce(input)
    expect(output).toBe('if (false) {    \u2028     }\u2028const after = 1')
    expect(output.length).toBe(input.length)
  })

  it('keeps line separators in a non-block dead body', () => {
    const input = 'if (false) a +\u2028b()'
    const output = dce(input)
    expect(output).toBe('if (false) ;  \u2028   ')
    expect(output.length).toBe(input.length)
  })

  it('keeps line separators on the UTF-16 entry too', () => {
    // a leading BOM routes the input to the UTF-16 entry points
    const input = '\uFEFFif (false) { a()\u2028 b() }'
    const output = dce(input)
    expect(output).toBe('\uFEFFif (false) {    \u2028     }')
  })
})

describe('dce: composition with the eraser', () => {
  it('blanks runtime TS inside the dead arm without expanding it', () => {
    const input = 'if (false) { enum E { A } }'
    const output = dce(input)
    expect(output).toBe(`if (false) {${spaces(' enum E { A } ')}}`)
    expect(output).not.toContain('E[')
    expect(output.length).toBe(input.length)
  })

  it('still erases TS inside the kept arm', () => {
    const input = 'if (false) { a() } else { const x: number = 1; use(x) }'
    const output = dce(input)
    expect(output).toBe('if (false) {     } else { const x         = 1; use(x) }')
  })
})
