import ts from 'typescript'
import { describe, expect, it } from 'vitest'
import { transpile } from './parity'

// Each initializer returns a number so these cases exercise enum reference
// resolution without depending on string folding or invalid string enums.
const cases = [
  ['catch nested let does not escape its block', '(() => { try { throw 0 } catch (e) { { let A = 2 } return A } })()', 1],
  ['catch without a parameter does not absorb nested bindings', '(() => { try { throw 0 } catch { { let A = 2 } return A } })()', 1],
  ['catch parameter shadows the member', '(() => { try { throw 2 } catch (A) { return A } })()', 2],
  ['destructured catch parameter shadows the member', '(() => { try { throw { value: 2 } } catch ({ value: A }) { return A } })()', 2],
  ['catch parameter does not escape the catch', '(() => { try { throw 2 } catch (A) {} return A })()', 1],
  ['var in catch still hoists to the function body', '(() => { try { throw 0 } catch (e) { var A = 2 } return A })()', 2],
  ['arrow default cannot see body var', '((x = A) => { var A = 2; return x })()', 1],
  ['function default cannot see body var', '(function (x = A) { var A = 2; return x })()', 1],
  ['default closure retains the parameter environment', '((get = () => A) => { var A = 2; return get() })()', 1],
  ['earlier parameter shadows the member in a later default', '((A = 2, x = A) => x)()', 2],
  ['body var shadows the member after parameter evaluation', '((x = A) => { var A = 2; return x + A })()', 3],
  ['const initializer class name does not bind outside the class', '(() => { const C = class A {}; return A })()', 1],
  ['var initializer class name does not bind outside the class', '(() => { var C = class A {}; return A })()', 1],
  ['destructuring default class name does not bind outside the class', '(() => { const { x = class A {} } = {}; return A })()', 1],
  ['parameter default class name does not bind in the body', '((x = class A {}) => A)()', 1],
  ['destructured parameter default class name does not bind in the body', '(({ x = class A {} } = {}) => A)()', 1],
  ['computed binding key resolves the member', '(() => { const { [A]: x } = { 1: 2 }; return x })()', 2],
  ['renamed destructuring binding shadows the member', '(() => { const { x: A } = { x: 2 }; return A })()', 2],
  ['class expression name binds inside a method', '(class A { static value() { return typeof A === "function" ? 2 : 3 } }).value()', 2],
  ['class expression name binds inside a field initializer', '(class A { static value = typeof A === "function" ? 2 : 3 }).value', 2],
  ['class expression name does not escape into a sibling expression', '((class A {}), A)', 1],
  ['static block var does not hoist to the enclosing function', '(() => { class C { static { var A = 2 } } return A })()', 1],
  ['static block var shadows the member inside the static block', '(() => { let result = 0; class C { static { var A = 2; result = A } } return result })()', 2],
  ['static block let shadows the member inside the static block', '(() => { let result = 0; class C { static { let A = 2; result = A } } return result })()', 2],
  ['separate static blocks do not share var bindings', '(() => { let result = 0; class C { static { var A = 2 } static { result = A } } return result })()', 1],
  ['static block enum declaration shadows the member', '(() => { class C { static value = 0; static { enum A { X = 2 } C.value = A.X } } return C.value })()', 2],
  ['for head binding does not escape the loop', '(() => { for (let A = 2; A < 3; A++) {} return A })()', 1],
  ['for of binding shadows the member in the body', '(() => { for (const A of [2]) { return A } return 0 })()', 2],
  ['switch binding is visible from a later case', '(() => { switch (0) { case 0: let A = 2; case 1: return A } return 0 })()', 2],
  ['switch nested block binding does not escape into another case', '(() => { switch (0) { case 0: { let A = 2 } case 1: return A } return 0 })()', 1],
  ['later parameter preserves its temporal dead zone', '(() => { try { return ((x = A, A = 2) => x)() } catch (error) { if (error instanceof ReferenceError) return 7; throw error } })()', 7],
  ['block binding shadows even before its declaration', '(() => { try { { return A; let A = 2 } } catch (error) { if (error instanceof ReferenceError) return 7; throw error } })()', 7],
  ['for of right side sees the head binding temporal dead zone', '(() => { try { for (let A of [A]) {} return 0 } catch (error) { if (error instanceof ReferenceError) return 7; throw error } })()', 7],
  ['switch case test sees the shared scope temporal dead zone', '(() => { try { switch (0) { case A: let A = 2; return A } return 0 } catch (error) { if (error instanceof ReferenceError) return 7; throw error } })()', 7],
  ['expression-bodied arrow parameter shadows the member', '((A = 2) => A)()', 2],
] as const

function evaluate(code: string): unknown {
  // Match the strict context used by the TypeScript reference output.
  return new Function(`"use strict";\n${code}\nreturn E.B;`)()
}

describe('enum initializer scope boundaries', () => {
  it.each(cases)('%s', (_label, initializer, expected) => {
    const input = `enum E { A = 1, B = ${initializer} }`
    const reference = ts.transpileModule(input, {
      compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.None },
    }).outputText

    // Check the explicit expectation against tsc before checking our emitter.
    // Runtime evaluation also catches syntactically invalid rewrites. The
    // shared transpile helper checks synchronous/asynchronous API parity.
    expect(evaluate(reference), 'TypeScript reference').toBe(expected)
    expect(evaluate(transpile(input)), 'native enum expansion').toBe(expected)
  })
})

// Whole-file companions to the initializer cases above: the binding that
// decides whether `B = A` folds needs declarations petrea erases or keeps
// around the reference. The fold decision is textual, so each case pins the
// emitted member value directly against an explicit expectation (checked
// against tsc first) — runtime evaluation alone cannot tell `B = 5` from a
// verbatim `B = A` that reads 5 at runtime. The three shadow cases would fail
// on v0.4.2's registry; the four preservation cases pin emitter-parity or
// v0.4.2 behavior that later registry work must not lose — including the
// deliberate ones: an ambient enum and a function-scoped ambient variable
// both keep blocking folds, because on the invalid inputs where the emitter
// varies by scope no rule matches it everywhere.
const foldDecisionCases = [
  ['type-only import does not shadow the const', 'const A = 5; import type { A } from "./x"; enum E { B = A }', '5'],
  ['inline type specifier does not shadow the const', 'const A = 5; import { type A } from "./x"; enum E { B = A }', '5'],
  ['namespace var stops at the namespace body', 'const V = 5; namespace N { export var V = 9 } enum E { B = V }', '5'],
  ['declare enum member still folds', 'declare enum D { X = 9 } enum E { B = D.X }', '9'],
  ['block-scoped declare enum name blocks the fold', 'const A = 5; { declare enum A { X } enum E { B = A } }', 'A'],
  ['function-scoped ambient variable blocks the fold', 'const A = 5; function f() { declare var A; enum E { B = A } return E.B }', 'A'],
  ['import-equals does not shadow the const', 'const A = 5; import A = require("./x"); enum E { B = A }', '5'],
] as const

function enumMemberValue(code: string): string {
  const match = code.match(/E\[E\["B"\] = ([^\]]+)\]/)
  expect(match, 'emitted enum member').toBeTruthy()
  return match[1]
}

function tscReference(input: string): string {
  return ts.transpileModule(input, {
    compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.None },
  }).outputText
}

describe('erased and surviving declarations in enum resolution', () => {
  it.each(foldDecisionCases)('%s', (_label, input, expected) => {
    expect(enumMemberValue(tscReference(input)), 'TypeScript reference').toBe(expected)
    expect(enumMemberValue(transpile(input)), 'native enum expansion').toBe(expected)
  })
})
