import type { TranspileOptions } from '../src/types'
import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { transpile as transpileOxidase } from 'oxidase'
import tsBlankSpace from 'ts-blank-space'
import { describe, it } from 'vitest'
import { transpile, transpileSync } from '../src/index'

const fixtureDir = join(dirname(fileURLToPath(import.meta.url)), '../test/fixture')

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

const enumHeavy = `enum Color { Red, Green = 5, Blue = Green + 2, Named = 'named' }
enum Flags { A = 1 << 0, B = 1 << 1, C = A | B, D = C + 1 }
const enum Private { X, Y = X * 2 }
`.repeat(25)

const samples: Array<{ name: string, input: string, options?: TranspileOptions, tsBlankSpaceThrows?: boolean }> = [
  {
    name: 'inline',
    input: `const a: number = 1;\nfunction greet(name: string): string { return \`hi \${name}\`; }\ninterface Shape { area(): number }\n`,
  },
  {
    name: 'fixture corpus (~15KB)',
    input: fixtureInput(),
  },
  {
    name: 'large (~100KB)',
    input: enlarged(fixtureInput(), 100_000),
  },
  {
    name: 'enum heavy',
    input: enumHeavy,
    // ts-blank-space cannot transform enums; with a no-op onError it keeps
    // them verbatim, so its numbers on this sample measure less work
    tsBlankSpaceThrows: true,
  },
]

describe('transpile', () => {
  for (const { name, input, options, tsBlankSpaceThrows } of samples) {
    // sanity: every implementation must accept the sample before timing it
    // (vitest warms each benchmark on its own)
    transpileOxidase(input)
    tsBlankSpace(input, tsBlankSpaceThrows ? () => {} : undefined)
    transpileSync(input, options)

    it(name, async ({ bench }) => {
      // vitest's summary baselines the fastest implementation per scenario;
      // oxidase (a no-AST Rust pipeline) is the reference point on small
      // inputs and enum-heavy transformations
      await bench('oxidase', () => {
        transpileOxidase(input)
      }).run()

      await bench('ts-blank-space', () => {
        tsBlankSpace(input, tsBlankSpaceThrows ? () => {} : undefined)
      }).run()

      await bench('transpileSync', () => {
        transpileSync(input, options)
      }).run()

      await bench('transpile (async)', async () => {
        await transpile(input, options)
      }).run()
    })
  }
})
