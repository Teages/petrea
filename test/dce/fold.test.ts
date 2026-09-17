import type { UnsupportedSyntax } from '../../src/index'
import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

function dce(input: string): { output: string, reports: UnsupportedSyntax[] } {
  const reports: UnsupportedSyntax[] = []
  const output = transpile(input, { dce: true, onError: node => reports.push(node) })
  return { output, reports }
}

describe('dce: truthiness fold domain', () => {
  it.each([
    ['false'],
    ['0'],
    ['0x0'],
    ['0e10'],
    ['\'\''],
    ['""'],
    ['null'],
    ['0n'],
    ['0x0n'],
    ['0b0n'],
  ])('folds the falsy spelling %s by its parsed value', (expr) => {
    const { output, reports } = dce(`if (${expr}) { a() }`)
    expect(output).toBe(`if (${expr}) {     }`)
    expect(reports).toEqual([])
  })

  it.each([
    ['true'],
    ['1'],
    ['\'0\''],
    ['1e999'],
    ['0x1'],
    ['1n'],
  ])('folds the truthy spelling %s and hollows the else arm', (expr) => {
    const { output, reports } = dce(`if (${expr}) { a() } else { b() }`)
    expect(output).toBe(`if (${expr}) { a() } else {     }`)
    expect(reports).toEqual([])
  })

  it('folds a string by its decoded value, not its source text', () => {
    // "\" + line continuation + "\" decodes to the empty string — falsy —
    // while any character of source text would read truthy
    const { output, reports } = dce('if ("\\\n") { a() }')
    expect(output).toBe('if ("\\\n") {     }')
    expect(reports).toEqual([])
  })

  it.each([
    'undefined',
    'NaN',
    'Infinity',
    'foo',
    'a.b',
    'flag ?? false',
    '!flag',
    'flag && true',
    'flag || false',
    'flag === false',
    '-""',
    '(log(), flag)',
  ])('never folds %s, truthy or not', (expr) => {
    // both arms present: a wrong truthy fold hollows the else arm and a
    // wrong falsy fold the consequent — the no-else form would hide the
    // truthy direction entirely
    const input = `if (${expr}) { a() } else { b() }`
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(reports).toEqual([])
  })

  it('sees through parens and transparent TS wrappers', () => {
    // the kept head still walks the eraser: `as`/`satisfies` blank away
    expect(dce('if ((false)) { a() }').output).toBe('if ((false)) {     }')
    expect(dce('if (false as boolean) { a() }').output).toBe(
      'if (false           ) {     }',
    )
    expect(dce('if (false satisfies boolean) { a() }').output).toBe(
      'if (false                  ) {     }',
    )
  })

  it('folds a falsy && short-circuit, keeping the unevaluated right side in the head', () => {
    const { output, reports } = dce('if (false && sideEffect()) { a() }')
    expect(output).toBe('if (false && sideEffect()) {     }')
    expect(reports).toEqual([])
  })

  it('folds a truthy || short-circuit and hollows the else arm', () => {
    const { output, reports } = dce('if (true || sideEffect()) { a() } else { b() }')
    expect(output).toBe('if (true || sideEffect()) { a() } else {     }')
    expect(reports).toEqual([])
  })

  it('folds a right-decided && to falsy while the left still evaluates', () => {
    // `x && false` is falsy whichever way x lands: the left operand still
    // evaluates in the kept head, so its side effects and throws stay
    const input = 'if (sideEffect() && false) { a() } else { b() }'
    const { output, reports } = dce(input)
    expect(output).toBe('if (sideEffect() && false) {     } else { b() }')
    expect(reports).toEqual([])
  })

  it('folds a right-decided || to truthy', () => {
    const input = 'if (sideEffect() || true) { a() } else { b() }'
    const { output, reports } = dce(input)
    expect(output).toBe('if (sideEffect() || true) { a() } else {     }')
    expect(reports).toEqual([])
  })

  it('chains right-decided proofs through nesting', () => {
    const input = 'if (flag && (sideEffect() && false)) { a() } else { b() }'
    const { output, reports } = dce(input)
    expect(output).toBe('if (flag && (sideEffect() && false)) {     } else { b() }')
    expect(reports).toEqual([])
  })

  it('keeps a proven left side whose right side decides verbatim', () => {
    const first = 'if (true && flag) { a() } else { b() }'
    expect(dce(first).output).toBe(first)
    const second = 'if (false || flag) { a() } else { b() }'
    expect(dce(second).output).toBe(second)
  })
})

describe('dce: fold domain — void, negation, unary minus', () => {
  it.each([
    ['if (void 0) { a() }', 'if (void 0) {     }'],
    ['if (void sideEffect()) { a() } else { b() }', 'if (void sideEffect()) {     } else { b() }'],
  ])('folds void of any operand to falsy: %s', (input, expected) => {
    // `void x` is undefined whichever way x lands; the kept head keeps the
    // operand's side effects and throws
    const { output, reports } = dce(input)
    expect(output).toBe(expected)
    expect(reports).toEqual([])
  })

  it('keeps the operand\'s throw when the output runs', () => {
    const output = dce('if (void fail()) { dead() }').output
    const marker = new Error('boom')
    expect(() => new Function('fail', output)(() => {
      throw marker
    })).toThrow(marker)
  })

  it.each([
    ['if (!false) { a() } else { b() }', 'if (!false) { a() } else {     }'],
    ['if (!0) { a() } else { b() }', 'if (!0) { a() } else {     }'],
    ['if (!!false) { a() } else { b() }', 'if (!!false) {     } else { b() }'],
    ['if (!(flag && false)) { a() } else { b() }', 'if (!(flag && false)) { a() } else {     }'],
  ])('folds negation recursively: %s', (input, expected) => {
    const { output, reports } = dce(input)
    expect(output).toBe(expected)
    expect(reports).toEqual([])
  })

  it.each([
    ['if (-0) { a() } else { b() }', 'if (-0) {     } else { b() }'],
    ['if (-1) { a() } else { b() }', 'if (-1) { a() } else {     }'],
  ])('folds negated numeric literals: %s', (input, expected) => {
    const { output, reports } = dce(input)
    expect(output).toBe(expected)
    expect(reports).toEqual([])
  })
})

describe('dce: fold domain — strict equality', () => {
  it.each([
    ['"production" === "production"', 'else'],
    ['1 === 2', 'consequent'],
    ['1 !== 2', 'else'],
    ['"" === ""', 'else'],
    ['true === false', 'consequent'],
    ['false === false', 'else'],
    ['1 !== 1', 'consequent'],
    ['"a" !== "a"', 'consequent'],
    ['0x1n === 1n', 'else'],
    // 2^53 and 2^53+1 collide as f64: a float-mediated compare calls them
    // equal, a BigInt compare does not
    ['9007199254740992n === 9007199254740993n', 'consequent'],
    ['-0 === 0', 'else'],
    ['1 === "1"', 'consequent'],
    ['1 === 1n', 'consequent'],
    ['true === 1', 'consequent'],
    ['null !== 0', 'else'],
    ['null === null', 'else'],
    ['void 0 === void 0', 'else'],
    // `===` distinguishes what `==` equates
    ['null === void 0', 'consequent'],
  ])('folds %s over the literal domain', (expr, dead) => {
    const input = `if (${expr}) { a() } else { b() }`
    const expected = dead === 'consequent'
      ? `if (${expr}) {     } else { b() }`
      : `if (${expr}) { a() } else {     }`
    const { output, reports } = dce(input)
    expect(output).toBe(expected)
    expect(reports).toEqual([])
  })

  it.each([
    // a known truthiness does not prove a known value: `flag && false`
    // yields 0 for `flag = 0`, an object for `flag || true` with `flag = {}`
    '(flag && false) === false',
    '(flag || true) === true',
    // the shadowable identifier is never special-cased inside a comparison
    'null === undefined',
  ])('never folds %s — truthiness is not value', (expr) => {
    const input = `if (${expr}) { a() } else { b() }`
    const { output, reports } = dce(input)
    expect(output).toBe(input)
    expect(reports).toEqual([])
  })

  it('compares strings by decoded value, not source spelling', () => {
    expect(dce('if ("a\\u0062c" === "abc") { a() } else { b() }').output).toBe(
      'if ("a\\u0062c" === "abc") { a() } else {     }',
    )
  })

  it('judges raw lone surrogates unequal on the UTF-16 path', () => {
    // two different raw lone surrogates are different strings: a naive
    // compare over the lossy parse copy would see both as U+FFFD, call
    // them equal, and hollow the wrong arm — the input routes to the
    // UTF-16 entry, where the original units decide
    const high = String.fromCharCode(0xD800)
    const other = String.fromCharCode(0xDBFF)
    const input = `if ("${high}" === "${other}") { a() } else { b() }`
    const { output, reports } = dce(input)
    expect(output).toBe(`if ("${high}" === "${other}") {     } else { b() }`)
    expect(reports).toEqual([])
  })

  it('judges a raw lone surrogate equal to its escaped spelling', () => {
    // the raw unit and "\uD800" decode to the same value — comparing raw
    // source fragments would call them different
    const high = String.fromCharCode(0xD800)
    const input = `if ("${high}" === "\\uD800") { a() } else { b() }`
    const { output, reports } = dce(input)
    expect(output).toBe(`if ("${high}" === "\\uD800") { a() } else {     }`)
    expect(reports).toEqual([])
  })

  it('distinguishes a raw lone surrogate from the real replacement character', () => {
    // the corruption artifact (U+FFFD in the parse copy) is not the same
    // unit as the raw surrogate it replaced: the right side decodes to the
    // real replacement character, the left holds the raw unit
    const high = String.fromCharCode(0xD800)
    const input = `if ("${high}" === "\\uFFFD") { a() } else { b() }`
    const { output, reports } = dce(input)
    expect(output).toBe(`if ("${high}" === "\\uFFFD") {     } else { b() }`)
    expect(reports).toEqual([])
  })
})

describe('dce: fold domain — sequence tails', () => {
  it.each([
    ['if ((sideEffect(), false)) { a() } else { b() }', 'if ((sideEffect(), false)) {     } else { b() }'],
    ['if (sideEffect(), "") { a() }', 'if (sideEffect(), "") {     }'],
    ['if ((log(), sideEffect(), 1)) { a() } else { b() }', 'if ((log(), sideEffect(), 1)) { a() } else {     }'],
  ])('folds a sequence by its last element only: %s', (input, expected) => {
    // every earlier element still evaluates in the kept head — its side
    // effects, throws and awaits all stay
    const { output, reports } = dce(input)
    expect(output).toBe(expected)
    expect(reports).toEqual([])
  })
})
