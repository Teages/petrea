import type { UnsupportedSyntax } from '../../src/index'
import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

/** Every non-newline code unit of a blanked region becomes a space. */
const spaces = (text: string) => text.replace(/[^\r\n]/g, ' ')

function dce(input: string): { output: string, reports: UnsupportedSyntax[] } {
  const reports: UnsupportedSyntax[] = []
  const output = transpile(input, { dce: true, onError: node => reports.push(node) })
  return { output, reports }
}

const kinds = (reports: UnsupportedSyntax[]) => reports.map(node => node.type)

describe('dce: hoisting guard', () => {
  it('keeps a dead arm that declares a var, verbatim, reported dce-hoisted', () => {
    const input = 'function f() {\n  if (false) { var x = 1 }\n  return x\n}'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(kinds(reports)).toContain('dce-hoisted')
    // the hoisted binding still exists: reading it yields undefined instead
    // of the ReferenceError a blanked declaration would throw
    const f = new Function(`${output}\nreturn f`)()
    expect(f()).toBeUndefined()
  })

  it('catches a var inside a for initializer of the dead arm', () => {
    const input = 'if (false) { for (var i = 0; i < 2; i++) {} }'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(kinds(reports)).toContain('dce-hoisted')
  })

  it('keeps a dead arm that declares a function', () => {
    const input = 'if (false) { function g() {} }'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(kinds(reports)).toContain('dce-hoisted')
  })

  it('guards a non-block dead body and a dead else arm alike', () => {
    for (const input of ['if (false) var x', 'if (true) { a() } else { var x }']) {
      const { output, reports } = dce(input)
      expect(output, input).toBe(input)
      expect(kinds(reports)).toContain('dce-hoisted')
    }
  })

  it('refuses conservatively on a var inside a nested function', () => {
    // the flat scan does not reason about scopes: it refuses and loses the
    // fold rather than decide where that var hoists to
    const input = 'if (false) { (() => { var x })() }'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(kinds(reports)).toContain('dce-hoisted')
  })

  it('never scans the live arm', () => {
    // only the dead consequent folds; the else arm's var is live code
    const input = 'if (false) { a() } else { var x }'
    const { output, reports } = dce(input)
    expect(output).toBe('if (false) {     } else { var x }')
    expect(reports).toEqual([])
  })

  it('hollows block-scoped declarations freely', () => {
    const input = 'if (false) { let a = 1; const b = 2; class C {} }'
    const { output, reports } = dce(input)
    expect(output).toBe(`if (false) {${spaces(' let a = 1; const b = 2; class C {} ')}}`)
    expect(output.length).toBe(input.length)
    expect(reports).toEqual([])
  })
})

describe('dce: top-level await guard', () => {
  it.each([
    'if (false) { await init() }',
    'if (false) { for await (const x of y) {} }',
    'if (false) { await using x = y }',
    'while (false) { await x }',
    'if (true) { a() } else { await x }',
    'if (false) await x',
  ])('keeps module-top-level await verbatim, reported dce-await: %s', (input) => {
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(kinds(reports)).toContain('dce-await')
  })

  it('treats an await in a class computed key as top level', () => {
    // the computed key evaluates in the enclosing (module top) scope, not
    // inside the method's function scope
    const input = 'if (false) { class C { [await key()]() {} } }'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(kinds(reports)).toContain('dce-await')
  })

  it('keeps an await in the kept test, hollowing the body', () => {
    // the shell preserves the head verbatim: its await survives the fold,
    // so the static module property survives too — no dce-await report
    const { output, reports } = dce('if (false && await init()) { a() }')
    expect(output).toBe('if (false && await init()) {     }')
    expect(reports).toEqual([])
  })

  it('keeps an await inside a kept comma-headed test', () => {
    // same rule as the short-circuit head: the test survives verbatim, so
    // its await keeps the module's top-level-await property
    const { output, reports } = dce('if ((await x, false)) { a() }')
    expect(output).toBe('if ((await x, false)) {     }')
    expect(reports).toEqual([])
  })

  it('never scans the live arm for awaits', () => {
    const input = 'if (false) { a() } else { await x }'
    const { output, reports } = dce(input)
    expect(output).toBe('if (false) {     } else { await x }')
    expect(reports).toEqual([])
  })

  it('hollows awaits inside function expressions and methods', () => {
    for (const interior of [
      ' consume(async function () { await x }) ',
      ' const o = { async m() { await x } } ',
    ]) {
      const input = `if (false) {${interior}}`
      const { output, reports } = dce(input)
      expect(output, input).toBe(`if (false) {${spaces(interior)}}`)
      expect(reports).toEqual([])
    }
  })

  it('hollows the in-source testing shape: await nested in a callback', () => {
    const input = 'if (false) { it("x", async () => { await db() }) }'
    const { output, reports } = dce(input)
    expect(output).toBe(`if (false) {${spaces(' it("x", async () => { await db() }) ')}}`)
    expect(reports).toEqual([])
  })

  it('hollows an await inside an async function wrapper', () => {
    const input = 'async function f() { if (false) { await x } }'
    const { output, reports } = dce(input)
    expect(output).toBe(`async function f() { if (false) {${spaces(' await x ')}} }`)
    expect(reports).toEqual([])
  })
})

describe('dce: a refused fold still walks the eraser', () => {
  it('erases TypeScript inside a hoisting-refused fold', () => {
    const { output, reports } = dce('if (false) { var x: number = 1 } else { const y: number = 2 }')
    expect(output).toBe('if (false) { var x         = 1 } else { const y         = 2 }')
    expect(kinds(reports)).toContain('dce-hoisted')
  })

  it('erases the test and the arm of an await-refused fold', () => {
    const { output, reports } = dce('if (false as boolean) { await (x as Promise<void>) }')
    expect(output).toBe('if (false           ) { await (x                 ) }')
    expect(kinds(reports)).toContain('dce-await')
  })

  it('erases TypeScript inside silently refused shapes', () => {
    const { output, reports } = dce('if (flag) { const x: number = 1 }')
    expect(output).toBe('if (flag) { const x         = 1 }')
    expect(reports).toEqual([])
  })
})

describe('dce: report spans', () => {
  it('reports the candidate statement span', () => {
    const input = '  if (false) { var x = 1 }'
    const { reports } = dce(input)
    const report = reports.find(node => node.type === 'dce-hoisted')
    expect(report?.start).toBe(input.indexOf('if'))
    expect(report?.end).toBe(input.length)
  })

  it('reports offsets in UTF-16 code units', () => {
    // € inside the comment is one code unit but three UTF-8 bytes: a byte
    // offset would land two past the `if`
    const input = '// €\nif (false) { var x = 1 }'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    const report = reports.find(node => node.type === 'dce-hoisted')
    expect(report?.start).toBe(input.indexOf('if'))
    expect(report?.end).toBe(input.length)
  })
})

describe('dce: stays inert without the option', () => {
  it.each([
    'if (false) { var x }',
    'if (false) { await x }',
    'if (false) { a() }',
  ])('reports nothing for %s when dce is off', (input) => {
    const reports: UnsupportedSyntax[] = []
    transpile(input, { onError: node => reports.push(node) })
    expect(reports).toEqual([])
  })
})

describe('dce: guard scan budget', () => {
  it('caps the rescan amplification at one dce-budget report', () => {
    // a tower of refused folds rescans its shared descendants — the
    // global budget stops the amplification: every level stays verbatim,
    // one dce-budget report fires at the first candidate that ran dry,
    // and no report follows it
    const depth = 400
    const input = `${'if(false){'.repeat(depth)}var x;${'}'.repeat(depth)}`
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    const kinds = reports.map(node => node.type)
    expect(kinds.filter(kind => kind === 'dce-budget')).toHaveLength(1)
    // nothing reports after the budget trips
    expect(kinds.slice(kinds.lastIndexOf('dce-budget') + 1)).toEqual([])
  })

  it('never trips on acceptance: one sweep swallows the whole nest', () => {
    // a clean tower folds once at the outermost candidate — the interior
    // is never revisited, so even deep nesting stays far under the budget
    const depth = 300
    const input = `${'if(false){'.repeat(depth)}a();${'}'.repeat(depth)}`
    const { output, reports } = dce(input)
    expect(reports).toEqual([])
    expect(output).toBe(`if(false){${' '.repeat(input.length - 'if(false){'.length - 1)}}`)
    expect(output.length).toBe(input.length)
  })

  it('spends nothing on live code, however deep', () => {
    // only folding candidates sweep: deeply nested live guards cost no
    // budget at all
    const depth = 60
    const input = `${'if (user) {'.repeat(depth)}work();${'}'.repeat(depth)}`
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(reports).toEqual([])
  })
})

describe('dce: report-channel changes from wider folding', () => {
  it('fires the hoisting guard once a right-decided fold meets a var arm', () => {
    // before the fold domain widened, this test never folded and no report
    // fired; now the fold is recognized and the guard answers it — an API
    // behavior change, pinned deliberately
    const input = 'if (sideEffect() && false) { var x }'
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(reports.map(node => node.type)).toEqual(['dce-hoisted'])
  })

  it('stops walking a dead arm once the fold happens', () => {
    // the eraser no longer reaches the dead body, so its
    // TSParameterProperty report disappears along with its text — an API
    // behavior change, pinned deliberately
    const input
      = 'if (sideEffect() && false) { class C { constructor(public x: number) {} } }'
    const { output, reports } = dce(input)
    expect(output).toBe(
      `if (sideEffect() && false) {${spaces(' class C { constructor(public x: number) {} } ')}}`,
    )
    expect(reports).toEqual([])
    // the contrast: with dce off the same input walks the arm and reports
    const contrast: UnsupportedSyntax[] = []
    transpile(input, { onError: node => contrast.push(node) })
    expect(contrast.map(node => node.type)).toEqual(['TSParameterProperty'])
  })
})
