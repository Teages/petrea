import type { UnsupportedSyntax } from '../../src/index'
import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

function dce(input: string): { output: string, reports: UnsupportedSyntax[] } {
  const reports: UnsupportedSyntax[] = []
  const output = transpile(input, { dce: true, onError: node => reports.push(node) })
  return { output, reports }
}

describe('dce: for statements', () => {
  it('hollows a falsy body, keeping init, test and update verbatim', () => {
    // the head all still evaluates: init runs, the test folds false, the
    // update never does — exactly what the source did
    const input = 'for (i = 0; false; i++) { a() }'
    const { output, reports } = dce(input)
    expect(output).toBe('for (i = 0; false; i++) {     }')
    expect(output.length).toBe(input.length)
    expect(reports).toEqual([])
    // a bare head folds the same way
    expect(dce('for (;false;) { a() }').output).toBe('for (;false;) {     }')
  })

  it('treats a missing or truthy test as live', () => {
    expect(dce('for (;;) { a() }').output).toBe('for (;;) { a() }')
    expect(dce('for (; true;) { a() }').output).toBe('for (; true;) { a() }')
  })

  it('replaces a non-block falsy body with an empty statement', () => {
    expect(dce('for (i = 0; false; i++) a()').output).toBe(
      'for (i = 0; false; i++) ;  ',
    )
  })

  it('walks init, test and update through the eraser', () => {
    // all three head positions carry erasable TS: each must blank — an
    // implementation that visits only the init would still pass a
    // one-position case
    const input = 'for (let x: number = init(); false as boolean; tick(x as number)) { a() }'
    const { output, reports } = dce(input)
    expect(output).toBe(
      'for (let x         = init(); false           ; tick(x          )) {     }',
    )
    expect(output.length).toBe(input.length)
    expect(reports).toEqual([])
  })

  it('still reports constructs inside a never-executed update', () => {
    // the update never runs, but the transpiler still walks it
    const input = 'for (init(); false; (class { constructor(public x: number) {} })) { a() }'
    const { output, reports } = dce(input)
    expect(output).toBe(
      'for (init(); false; (class { constructor(public x        ) {} })) {     }',
    )
    expect(reports.map(node => node.type)).toEqual(['TSParameterProperty'])
  })

  it('runs exactly the init and the test — never update or body', () => {
    const input = [
      'const seen = []',
      'for (seen.push("init"); (seen.push("test"), false); seen.push("update")) {',
      '  seen.push("body")',
      '}',
    ].join('\n')
    const output = dce(input).output
    const seen = new Function(`${output}\nreturn seen`)()
    expect(seen).toEqual(['init', 'test'])
  })

  it('keeps a top-level await in the preserved update without a report', () => {
    // the guard reads the body only: an await in the kept head survives
    // the fold, so the module keeps its top-level-await property
    const input = 'for (init(); false; await tick()) { work() }'
    const { output, reports } = dce(input)
    expect(output).toBe(`for (init(); false; await tick()) {${' '.repeat(' work() '.length)}}`)
    expect(reports).toEqual([])
  })

  it('guards only the body: a var init never blocks the fold', () => {
    // `var i` hoists — but the head is preserved verbatim, so the binding
    // still initializes exactly as the source had it
    const input = 'for (var i = 0; false; i++) { a() }'
    const { output, reports } = dce(input)
    expect(output).toBe('for (var i = 0; false; i++) {     }')
    expect(reports).toEqual([])
  })

  it('refuses a body that declares a var', () => {
    const input = 'for (i = 0; false; i++) { var x }'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(reports.map(node => node.type)).toContain('dce-hoisted')
  })

  it('refuses a var body while the head still erases', () => {
    // the refusal is of the edit, never of the walk: every type position
    // in the head and the arm still blanks
    const input = 'for (let i: number = 0; false; tick(i as number)) { var x: number }'
    const { output, reports } = dce(input)
    expect(output).toBe('for (let i         = 0; false; tick(i          )) { var x         }')
    expect(reports.map(node => node.type)).toEqual(['dce-hoisted'])
  })

  it('guards a module-top-level await in the body', () => {
    const input = 'for (init(); false; tick()) { await x }'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(reports.map(node => node.type)).toContain('dce-await')
  })

  it('hollows a body await inside an async function wrapper', () => {
    const input = 'async function f() { for (init(); false; tick()) { await x } }'
    const { output, reports } = dce(input)
    expect(output).toBe('async function f() { for (init(); false; tick()) {         } }')
    expect(reports).toEqual([])
  })

  it('never touches for-in and for-of', () => {
    expect(dce('for (const x of []) { a() }').output).toBe('for (const x of []) { a() }')
    expect(dce('for (const k in o) { a() }').output).toBe('for (const k in o) { a() }')
    expect(dce('for await (const x of y) { a() }').output).toBe(
      'for await (const x of y) { a() }',
    )
  })
})
