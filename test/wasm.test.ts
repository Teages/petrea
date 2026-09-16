// Behavioral checks for the wasm build of the Rust pipeline, loaded through
// the Node-side wasm loader shipped in @petrea/binding-wasm32-wasip1. The
// wasm entry (dist/wasm.mjs) imports the same package's ESM loader,
// but that fetches over HTTP, so it is exercised separately through a fetch
// shim (browser-entry.test.ts).
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { transpileSync } from '../src/index'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const wasmLoaderPath = join(root, 'npm/wasm32-wasip1/binding.wasip1.cjs')

if (!existsSync(wasmLoaderPath)) {
  throw new Error('the wasm binding is missing; run `pnpm build` first')
}

interface WasmBinding {
  transpileNativeSync: (input: string, options?: object) => {
    code: string
    unsupported: Array<{ nodeType: string, start: number, end: number }>
  }
  transpileUtf16Sync: (units: Uint16Array, options?: object) => { code: Uint16Array }
}

const binding = createRequire(import.meta.url)(wasmLoaderPath) as WasmBinding

function toUnits(input: string): Uint16Array {
  return Uint16Array.from(input.split('').map(c => c.charCodeAt(0)))
}

describe('wasm binding', () => {
  it('blanks type annotations while preserving positions', () => {
    expect(binding.transpileNativeSync('const a: number = 1', { filename: 'a.ts' }).code)
      .toBe('const a         = 1')
  })

  it('blanks TS syntax inside JSX in tsx mode', () => {
    expect(binding.transpileNativeSync('const el = <div>{v as string}</div>\n', { lang: 'tsx' }).code)
      .toBe('const el = <div>{v          }</div>\n')
  })

  it('expands enums and evaluates like the TypeScript emitter', () => {
    const output = binding.transpileNativeSync('enum Color { Red, Green = 5, Blue }').code
    expect(output).toBe(
      'var  Color; (function (Color) { Color[Color["Red"] = 0] = "Red"; Color[Color["Green"] = 5] = "Green"; Color[Color["Blue"] = 6] = "Blue" })(Color || (Color = {}));',
    )
    const Color = new Function(`${output}; return Color;`)()
    expect(Color).toEqual({ Red: 0, Green: 5, Blue: 6, 0: 'Red', 5: 'Green', 6: 'Blue' })
  })

  it('formats doubles with JS shortest round-trip digits (ties included)', () => {
    // 1 - Number.EPSILON / 2 is a round-half-to-even tie that must print as
    // 0.9999999999999999, not 1
    for (const value of [0.1, 1e21, 1e-7, 1 - Number.EPSILON / 2, Number('1381472817847324.2')]) {
      const output = binding.transpileNativeSync(`enum E { A = ${String(value)} }`).code
      expect(output).toContain(`E["A"] = ${String(value)}]`)
    }
  })

  it('round-trips UTF-16 inputs with astral characters losslessly', () => {
    const input = 'const b: string = "😀"'
    const { code } = binding.transpileUtf16Sync(toUnits(input))
    expect(String.fromCharCode(...code)).toBe('const b         = "😀"')
  })

  it('rejects parse errors with a codeframe-carrying message', () => {
    let message = ''
    try {
      binding.transpileNativeSync('let x: = 1', { filename: 'app.js' })
    }
    catch (error) {
      message = (error as Error).message
    }
    expect(message).toContain('failed to parse app.js:')
    expect(message).toContain(',-[app.js:')
  })

  it('matches the platform binary on the fixture corpus', () => {
    const fixtureDir = join(root, 'test/fixture')
    for (const file of readdirSync(fixtureDir).filter(
      f => f.endsWith('.ts') || f.endsWith('.tsx'),
    )) {
      const input = readFileSync(join(fixtureDir, file), 'utf8')
      const options = file.endsWith('.tsx') ? { lang: 'tsx' } : undefined
      const wasmOutput = binding.transpileNativeSync(input, options).code
      expect(wasmOutput, `wasm output for ${file}`).toBe(transpileSync(input, options))
    }
  })

  it('hollows a dead block through dce like the platform binary', () => {
    expect(binding.transpileNativeSync('if (false) { a() }', { dce: true }).code)
      .toBe('if (false) {     }')
    expect(transpileSync('if (false) { a() }', { dce: true })).toBe('if (false) {     }')
  })

  it('reports a dce guard on the wasm build like the platform binary', () => {
    const input = 'if (false) { var x }'
    const wasmResult = binding.transpileNativeSync(input, { dce: true })
    expect(wasmResult.unsupported.map(node => node.nodeType)).toContain('dce-hoisted')
    const reports: Array<{ type: string }> = []
    transpileSync(input, { dce: true, onError: node => reports.push(node) })
    expect(reports.map(node => node.type)).toContain('dce-hoisted')
  })

  it('hollows through the UTF-16 entry with dce on', () => {
    const input = '\uFEFFif (false) { a() }'
    const { code } = binding.transpileUtf16Sync(toUnits(input), { dce: true })
    expect(String.fromCharCode(...code)).toBe('\uFEFFif (false) {     }')
  })
})
