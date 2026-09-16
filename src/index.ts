import type { NativeBinding } from './api'
import type { TranspileOptions } from './types'
import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { createApi } from './api'

export type { OnError, TranspileOptions, UnsupportedSyntax } from './types'

/** napi platform suffix for the running runtime, e.g. `darwin-arm64`. */
function platformSuffix(): string {
  const base = `${process.platform}-${process.arch}`
  switch (process.platform) {
    case 'linux': {
      // process.report carries the glibc version only when linked against
      // glibc; the other libc variant is not a fallback — it cannot load.
      const report = process.report as
        | { getReport?: () => { header?: { glibcVersionRuntime?: string } } }
        | undefined
      return report?.getReport?.().header?.glibcVersionRuntime === undefined
        ? `${base}-musl`
        : `${base}-gnu`
    }
    case 'win32':
      return `${base}-msvc`
    default:
      return base
  }
}

/**
 * The shipped `@petrea/binding-<suffix>` optional dependency for the running
 * platform, or undefined when none matches — loading a binary built for
 * another platform or libc would fail anyway, just with a confusing
 * dynamic-linking error.
 */
function platformPackage(suffix: string): string | undefined {
  const available = new Set([
    'darwin-arm64',
    'darwin-x64',
    'linux-arm64-gnu',
    'linux-arm64-musl',
    'linux-x64-gnu',
    'linux-x64-musl',
    'win32-arm64-msvc',
    'win32-x64-msvc',
  ])
  return available.has(suffix) ? `@petrea/binding-${suffix}` : undefined
}

/**
 * Locate the napi binding: first the installed `@petrea/binding-<suffix>`
 * optional dependency (exactly one ships in a normal install), then the
 * repo-local build under `binaries/` (development and CI). Returns undefined
 * when no usable binding exists; a present-but-broken one throws, which the
 * API layer keeps as the `cause` of its "no usable binding" error.
 */
function loadNodeBinding(): NativeBinding | undefined {
  const suffix = platformSuffix()
  const require = createRequire(import.meta.url)
  const pkg = platformPackage(suffix)
  let packageError: unknown
  if (pkg) {
    try {
      return require(pkg)
    }
    catch (error) {
      // fall through to the local build, remembering why the package failed
      packageError = error
    }
  }
  // '../binaries' sits next to the bundled dist/ and the src/ sources alike
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
  const binary = join(root, 'binaries', `binding.${suffix}.node`)
  if (existsSync(binary)) {
    return require(binary)
  }
  if (packageError !== undefined) {
    throw packageError
  }
  return undefined
}

const api = createApi(loadNodeBinding)

/**
 * Whether a native binding matching this platform was found. When false,
 * {@link transpile} and {@link transpileSync} throw on use; the WebAssembly
 * build (`petrea/wasm`) is the alternative.
 */
export const nativeBindingAvailable = api.isAvailable

export const transpile: (input: string, options?: TranspileOptions) => Promise<string>
  = api.transpile

export const transpileSync: (input: string, options?: TranspileOptions) => string
  = api.transpileSync
