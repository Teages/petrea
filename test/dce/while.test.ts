import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

const dce = (input: string) => transpile(input, { dce: true })

describe('dce: while statements', () => {
  it('hollows a falsy loop body in place', () => {
    const input = 'while (false) { a() }'
    const output = dce(input)
    expect(output).toBe('while (false) {     }')
    expect(output.length).toBe(input.length)
  })

  it('keeps newlines while hollowing a falsy body', () => {
    expect(dce('while (false) {\n  a()\n}')).toBe('while (false) {\n     \n}')
  })

  it('replaces a non-block falsy body with an empty statement', () => {
    expect(dce('while (false) a()')).toBe('while (false) ;  ')
  })

  it('keeps a truthy or unfolded loop untouched', () => {
    expect(dce('while (true) { a() }')).toBe('while (true) { a() }')
    expect(dce('while (cond) { a() }')).toBe('while (cond) { a() }')
  })

  it('never eliminates a do-while: its body runs once', () => {
    expect(dce('do { a() } while (false)')).toBe('do { a() } while (false)')
  })

  it('still folds candidates nested inside constructs it never touches', () => {
    // skipping a construct never means skipping its children
    expect(dce('do { if (false) { a() } } while (false)')).toBe(
      'do { if (false) {     } } while (false)',
    )
    expect(dce('for (;;) { if (false) { a() } }')).toBe(
      'for (;;) { if (false) {     } }',
    )
    expect(dce('switch (x) { case 1: if (false) { a() } }')).toBe(
      'switch (x) { case 1: if (false) {     } }',
    )
  })
})
