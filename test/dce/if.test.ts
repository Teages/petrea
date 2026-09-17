import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

/** Every non-newline byte of a blanked region becomes a space. */
const spaces = (text: string) => text.replace(/[^\n]/g, ' ')

const dce = (input: string) => transpile(input, { dce: true })

describe('dce: if statements', () => {
  it('hollows a falsy block in place', () => {
    const input = 'if (false) { a() }'
    const output = dce(input)
    expect(output).toBe('if (false) {     }')
    expect(output.length).toBe(input.length)
  })

  it('keeps newlines while hollowing a falsy block', () => {
    const input = 'if (false) {\n  a()\n}'
    const output = dce(input)
    expect(output).toBe(`if (false) {\n${spaces('  a()')}\n}`)
    expect(output.length).toBe(input.length)
    expect(output.split('\n').length).toBe(input.split('\n').length)
  })

  it('replaces a non-block dead body with an empty statement', () => {
    expect(dce('if (false) a()')).toBe('if (false) ;  ')
    // the blank starts at the body, so a leading line break stays put
    expect(dce('if (false)\n  a()')).toBe('if (false)\n  ;  ')
  })

  it('hollows only the dead arm of an if/else', () => {
    expect(dce('if (false) { a() } else { b() }')).toBe(
      'if (false) {     } else { b() }',
    )
    expect(dce('if (true) { a() } else { b() }')).toBe(
      'if (true) { a() } else {     }',
    )
  })

  it('walks else-if chains arm by arm', () => {
    expect(dce('if (false) { a() } else if (false) { b() } else { c() }')).toBe(
      'if (false) {     } else if (false) {     } else { c() }',
    )
    expect(dce('if (false) { a() } else if (true) { b() } else { c() }')).toBe(
      'if (false) {     } else if (true) { b() } else {     }',
    )
  })

  it('never blanks a dead else-if chain: the outer fold refuses', () => {
    // the dead alternate is not a block, so the outer fold refuses — and
    // the walk goes on, letting the chain's own candidates fold
    expect(dce('if (true) { a() } else if (flag) { b() } else { c() }')).toBe(
      'if (true) { a() } else if (flag) { b() } else { c() }',
    )
    expect(dce('if (true) { a() } else if (false) { b() } else { c() }')).toBe(
      'if (true) { a() } else if (false) {     } else { c() }',
    )
  })

  it('folds the outer block while a bare arm deeper in the chain refuses', () => {
    expect(dce('if (false) { a() } else if (flag) b(); else { c() }')).toBe(
      'if (false) {     } else if (flag) b(); else { c() }',
    )
  })

  it('hollows the dead block beside a non-block live arm', () => {
    // only the dead arm needs to be a block: the live arm keeps whatever
    // shape it had, walked like the default walk would
    expect(dce('if (true) x(); else { y() }')).toBe('if (true) x(); else {     }')
    expect(dce('if (false) { x() } else y()')).toBe('if (false) {     } else y()')
  })

  it('refuses an else whose dead arm is not a block', () => {
    // a non-block consequent needs its own `;` before `else` to parse
    expect(dce('if (false) a(); else b()')).toBe('if (false) a(); else b()')
    expect(dce('if (true) { a() } else b()')).toBe('if (true) { a() } else b()')
  })

  it('leaves a truthy if without else untouched', () => {
    expect(dce('if (true) { a() }')).toBe('if (true) { a() }')
    expect(dce('if (true) a()')).toBe('if (true) a()')
  })

  it('leaves an empty dead consequent untouched', () => {
    expect(dce('if (false);')).toBe('if (false);')
  })

  it('swallows nested dead ifs with the outer fold', () => {
    const input = 'if (false) { if (false) { a() } }'
    const output = dce(input)
    expect(output).toBe(`if (false) {${spaces(' if (false) { a() } ')}}`)
    expect(output).not.toContain('a()')
    expect(output.length).toBe(input.length)
  })
})
