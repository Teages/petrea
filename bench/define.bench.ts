import type { NativeBinding } from '../src/api'
import type { TranspileOptions } from '../src/types'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { bench, describe } from 'vitest'
import { transpileSync } from '../src/index'

const rootDir = join(dirname(fileURLToPath(import.meta.url)), '..')
const fixtureDir = join(rootDir, 'test/fixture')

/**
 * Raw `main`-branch binding for the no-define baseline, produced once with:
 * `git worktree add /tmp/petrea-main main && CARGO_TARGET_DIR=<repo>/native/target
 * cargo build --release && cp native/target/release/libpetrea_binding.dylib
 * binaries/main-reference.darwin-arm64.node`. The wrapper cannot host it
 * (main's result has no `warnings` field), so both sides of the comparison
 * call the raw napi entries — skipping the wrapper's surrogate scan equally.
 * The row is skipped with a note when the file is absent.
 */
const mainReferencePath = process.env.MAIN_REFERENCE_BINDING
  ?? join(rootDir, 'binaries', `main-reference.${process.platform}-${process.arch}.node`)

function loadRawBinding(path: string): NativeBinding | undefined {
  return existsSync(path) ? createRequire(import.meta.url)(path) as NativeBinding : undefined
}

const mainReference = loadRawBinding(mainReferencePath)
const branchRaw = loadRawBinding(join(rootDir, 'binaries', `binding.${process.platform}-${process.arch}.node`))

function fixtureInput(): string {
  return readdirSync(fixtureDir)
    .filter(file => file.endsWith('.ts'))
    .map(file => readFileSync(join(fixtureDir, file), 'utf8'))
    .join('\n')
}

/** Repeat until the input reaches roughly `target` characters. */
function enlarged(input: string, target: number): string {
  let out = input
  while (out.length < target) {
    out += input
  }
  return out
}

// One synthetic bundle-style block (~2KB): realistic TS with type annotations
// (the blanker's normal work) plus references to every hit-config key. The
// local `const __DEV__` keeps one shadowed site per block, exercising scope
// resolution on the no-replace path.
function globalsBlock(i: number): string {
  return `
interface CacheOptions${i} { verbose: boolean; level: number }

type Row${i} = { id: number; name: string; tags: string[] }

function isProd${i}(): boolean {
  return process.env.NODE_ENV === 'production'
}

function formatRows${i}(rows: Row${i}[]): string {
  return rows
    .filter((row): boolean => row.tags.length > 0)
    .map(row => \`\${row.id}: \${row.name} (\${row.tags.join(', ')})\`)
    .reduce((acc: string, line: string): string => (acc ? \`\${acc}\\n\${line}\` : line), '')
}

function parseRows${i}(input: string): Row${i}[] {
  const out: Row${i}[] = []
  for (const chunk of input.split(';')) {
    const [id, name, ...tags] = chunk.split(',')
    out.push({ id: Number(id), name, tags })
  }
  return out
}

class Cache${i}<T> {
  private entries: Map<string, T> = new Map()
  get(key: string): T | undefined {
    if (process.env.NODE_ENV !== 'production' && process.env.DEBUG) {
      console.log('cache miss', key)
    }
    return this.entries.get(key)
  }
  set(key: string, value: T): void {
    this.entries.set(key, value)
  }
}

const meta${i}: { version: string } = { version: process.env.APP_VERSION ?? '0.0.0' }

function bootstrap${i}(flags: CacheOptions${i}): string {
  const __DEV__ = flags.verbose
  if (__DEV__) {
    console.log('boot', meta${i}.version, formatRows${i}(parseRows${i}('1,a,x;b,y')))
  }
  window.__ANALYTICS__?.track('boot', { level: flags.level })
  if (isProd${i}()) {
    return 'prod:' + process.env.NODE_ENV
  }
  return 'dev'
}
`
}

// JSX variant: the same keys in expression positions inside components, so
// the JSX tag-position guards run on every hit.
function tsxBlock(i: number): string {
  return `
interface BadgeProps${i} { label: string; count: number }

function ProdTag${i}(): JSX.Element { return <span>prod</span> }

function Badge${i}(props: BadgeProps${i}) {
  const env: string = process.env.NODE_ENV
  if (__DEV__) {
    console.log('render', props.label)
  }
  return (
    <div className="badge" data-count={props.count}>
      <strong>{props.label}</strong>
      <small>{env} / {process.env.APP_VERSION}</small>
      {process.env.NODE_ENV === 'production' ? <ProdTag${i} /> : null}
      {process.env.DEBUG ? <code>dbg</code> : null}
      {window.__ANALYTICS__ ? <em>analytics</em> : null}
    </div>
  )
}
`
}

function repeated(block: (i: number) => string, count: number): string {
  return Array.from({ length: count }, (_, i) => block(i)).join('\n')
}

// Key counts and value shapes match between the miss and hit configs, so the
// two differ only in whether the keys occur in the dataset.
const defineMiss: Record<string, string> = {
  '__UNSET_FLAG__': 'true',
  'process.env.MISSING_MODE': '"missing"',
  'process.env.DEBUG_FLAG': 'false',
  'window.__MISSING_PHASE': '0',
  'import.meta.env.MISSING_BUILD': '\'miss\'',
}

const defineHit: Record<string, string> = {
  '__DEV__': 'true',
  'process.env.NODE_ENV': '"production"',
  'process.env.APP_VERSION': '"1.2.3"',
  'process.env.DEBUG': 'false',
  'window.__ANALYTICS__': 'false',
}

const BLOCKS = 48

const datasets: Array<{ name: string, input: string, lang?: 'tsx', expectHits: boolean }> = [
  { name: 'fixture corpus (~12KB, no hits possible)', input: fixtureInput() },
  { name: 'fixture corpus enlarged (~100KB, no hits possible)', input: enlarged(fixtureInput(), 100_000) },
  { name: `globals-heavy ts (~${Math.round(BLOCKS * 2.2)}KB, dense hits)`, input: repeated(globalsBlock, BLOCKS), expectHits: true },
  { name: `globals-heavy tsx (~${Math.round(BLOCKS * 1.1)}KB, dense hits)`, input: repeated(tsxBlock, BLOCKS), lang: 'tsx', expectHits: true },
]

function count(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1
}

describe('define overhead', () => {
  for (const { name, input, lang, expectHits } of datasets) {
    const base: TranspileOptions = lang ? { lang } : {}
    const miss: TranspileOptions = { ...base, define: defineMiss }
    const hit: TranspileOptions = { ...base, define: defineHit }

    // sanity, outside every timed run: the same input through each scenario
    const plainOut = transpileSync(input, base)
    const missOut = transpileSync(input, miss)
    const hitOut = transpileSync(input, hit)

    if (missOut !== plainOut) {
      throw new Error(`miss-config define changed the output on ${name}`)
    }
    if (expectHits) {
      const nodeEnvHits = count(input, 'process.env.NODE_ENV')
      if (count(hitOut, '"production"') < nodeEnvHits) {
        throw new Error(`hit config failed to replace NODE_ENV on ${name}`)
      }
      // every remaining __DEV__ occurrence must be a shadowed site: one
      // declaration plus one reference per ts block, none in tsx
      const devLeft = count(hitOut, '__DEV__')
      const devExpected = lang === 'tsx' ? 0 : 2 * BLOCKS
      if (devLeft !== devExpected) {
        throw new Error(`__DEV__ shadow handling off on ${name}: ${devLeft} left, ${devExpected} expected`)
      }
      if (hitOut === plainOut) {
        throw new Error(`hit config produced no changes on ${name}`)
      }
      process.stdout.write(
        `[define bench] ${name}: ${count(hitOut, '"production"')} NODE_ENV + ${count(hitOut, '"1.2.3"')} APP_VERSION replacements\n`,
      )
    }
    else if (hitOut !== plainOut) {
      throw new Error(`hit config changed the output on a dataset with no targets (${name})`)
    }

    // the define-free path must stay byte-identical to main; both raw-napi
    // sides skip the wrapper so the comparison is the native walk only
    const rawOptions = lang ? { lang } : undefined
    if (mainReference && branchRaw) {
      const mainOut = mainReference.transpileNativeSync(input, rawOptions).code
      const branchRawOut = branchRaw.transpileNativeSync(input, rawOptions).code
      if (mainOut !== plainOut || branchRawOut !== plainOut) {
        throw new Error(`define-free output drifted from main on ${name}`)
      }
      // the raw entries take the define map directly; same output as wrapped
      const rawMissOut = branchRaw.transpileNativeSync(input, { ...rawOptions, define: defineMiss }).code
      const rawHitOut = branchRaw.transpileNativeSync(input, { ...rawOptions, define: defineHit }).code
      if (rawMissOut !== missOut || rawHitOut !== hitOut) {
        throw new Error(`raw-napi define output differs from the wrapper on ${name}`)
      }
    }
    else if (!mainReference) {
      process.stdout.write(
        `[define bench] skipping the main baseline row: ${mainReferencePath} not found\n`,
      )
    }

    // native-only rows answer "what does define cost in the walk itself";
    // the public-api rows add the wrapper's full-input surrogate scan
    describe(`${name} · native (raw napi)`, () => {
      if (mainReference) {
        bench('main, no define', () => {
          mainReference.transpileNativeSync(input, rawOptions)
        })
      }

      if (branchRaw) {
        bench('branch, no define', () => {
          branchRaw.transpileNativeSync(input, rawOptions)
        })

        bench('branch, define no hits', () => {
          branchRaw.transpileNativeSync(input, { ...rawOptions, define: defineMiss })
        })

        bench('branch, define hits', () => {
          branchRaw.transpileNativeSync(input, { ...rawOptions, define: defineHit })
        })
      }
    })

    describe(`${name} · public api (transpileSync)`, () => {
      bench('no define', () => {
        transpileSync(input, base)
      })

      bench('define, no hits', () => {
        transpileSync(input, miss)
      })

      bench('define, hits', () => {
        transpileSync(input, hit)
      })
    })
  }
})
