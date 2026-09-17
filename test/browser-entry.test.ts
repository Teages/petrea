// Verifies the WebAssembly build at both of its layers:
//
// 1. dist/wasm.mjs — the published `petrea/wasm` entry. Node resolves its
//    bare `@petrea/wasm` import through the package's
//    `default` export condition (the CJS loader), so this layer proves the
//    wrapper and the dependency wiring, not the browser loading path itself.
// 2. npm/wasm/binding.wasip1-browser.js — the ESM loader bundlers
//    select via the `browser` export condition. It instantiates the wasm
//    module at import time through `globalThis.fetch`, which cannot read
//    file: URLs, so the test installs a fetch shim that serves local files.
//    This exercises the actual browser loading path: fetch → instantiate →
//    transpile.
import { existsSync, readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { afterAll, describe, expect, it } from 'vitest'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const browserEntryPath = join(root, 'dist/wasm.mjs')
const browserLoaderPath = join(root, 'npm/wasm/binding.wasip1-browser.js')

if (!existsSync(browserEntryPath)) {
  throw new Error('the browser entry is missing; run `pnpm build` first')
}
if (!existsSync(browserLoaderPath)) {
  throw new Error('the wasm binding is missing; run `pnpm build` first')
}

// the shim must be installed before the (top-level-await) imports below
const originalFetch = globalThis.fetch
const fetchedUrls: string[] = []
globalThis.fetch = (async (input: RequestInfo | URL) => {
  const url = String(input)
  if (url.startsWith('file:')) {
    fetchedUrls.push(url)
    return new Response(readFileSync(fileURLToPath(url)), { status: 200 })
  }
  return originalFetch(input)
}) as typeof fetch

afterAll(() => {
  globalThis.fetch = originalFetch
})

interface BrowserBinding {
  nativeBindingAvailable: boolean
  transpile: (input: string, options?: object) => Promise<string>
  transpileSync: (input: string, options?: object) => string
}

// imports the real browser loader: top-level await fetches the wasm through
// the shim above, then instantiation runs exactly as it would in a browser
const rawLoader = (await import(pathToFileURL(browserLoaderPath).href)) as {
  transpileAsync: (input: string, options?: object) => Promise<{ code: string }>
  transpileNativeSync: (input: string, options?: object) => { code: string }
}

// the loader exposes the raw napi surface; the public names are adapted by
// src/wasm.ts, mirrored here to reuse the same behavioral assertions
const browserLoader: BrowserBinding = {
  transpile: (input, options) => rawLoader.transpileAsync(input, options).then(r => r.code),
  transpileSync: (input, options) => rawLoader.transpileNativeSync(input, options).code,
}

// resolves through the package's `default` condition in Node (the CJS loader)
const browser = (await import(browserEntryPath)) as BrowserBinding

function assertBrowserBehavior(binding: BrowserBinding, label: string) {
  it(`[${label}] blanks type annotations while preserving positions`, () => {
    expect(binding.transpileSync('const a: number = 1')).toBe('const a         = 1')
  })

  it(`[${label}] blanks TS syntax inside JSX in tsx mode`, () => {
    expect(binding.transpileSync('const el = <div>{v as string}</div>\n', { lang: 'tsx' }))
      .toBe('const el = <div>{v          }</div>\n')
  })

  it(`[${label}] erases JSX element type arguments in tsx mode`, () => {
    expect(binding.transpileSync('const el = <Comp<T> x={v as string}/>\n', { lang: 'tsx' }))
      .toBe('const el = <Comp    x={v          }/>\n')
  })

  it(`[${label}] expands enums through the async entry`, async () => {
    const output = await binding.transpile('enum E { A = 2 }')
    expect(output).toContain('E[E["A"] = 2] = "A"')
  })

  it(`[${label}] blanks assertions inside JSX through the async tsx mode`, async () => {
    await expect(binding.transpile('const el = <div>{v as string}</div>\n', { lang: 'tsx' }))
      .resolves
      .toBe('const el = <div>{v          }</div>\n')
  })

  it(`[${label}] keeps astral characters lossless through the UTF-16 path`, () => {
    const input = 'const 文: string = "😀";'
    const output = binding.transpileSync(input)
    expect(output.length).toBe(input.length)
    expect(output).toContain('文')
    expect(output).toContain('😀')
  })
}

describe('wasm entry (dist/wasm.mjs)', () => {
  assertBrowserBehavior(browser, 'dist')

  // the SyntaxError wrapping is the public API layer's contract (src/api.ts)
  it('[dist] rejects parse errors with a codeframe-carrying SyntaxError', async () => {
    await expect(browser.transpile('let x: = 1')).rejects.toThrow(SyntaxError)
  })

  // export parity with the main entry: consumers may branch on the flag
  // regardless of which build the bundler selected
  it('[dist] reports the binding as available', () => {
    expect(browser.nativeBindingAvailable).toBe(true)
  })
})

describe('browser wasm loader (binding.wasip1-browser.js)', () => {
  assertBrowserBehavior(browserLoader, 'loader')

  // the raw loader rejects with a plain Error carrying the codeframe; the
  // SyntaxError wrapping happens one layer up in src/api.ts
  it('[loader] rejects parse errors with a codeframe message', async () => {
    let message = ''
    try {
      rawLoader.transpileNativeSync('let x: = 1', { filename: 'input.ts' })
    }
    catch (error) {
      message = (error as Error).message
    }
    expect(message).toContain('failed to parse input.ts:')
    expect(message).toContain(',-[input.ts:1:8]')
  })

  it('instantiates the wasm module through fetch', () => {
    expect(fetchedUrls.some(url => url.endsWith('binding.wasm32-wasip1.wasm'))).toBe(true)
  })
})
