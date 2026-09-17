import { execSync } from 'node:child_process'
// Builds the Rust binding for wasm32-wasip1 into the `wasm/` staging dir and
// copies the binding.* artifacts into the @petrea/wasm package dir
// (`npm/wasm`; the checked-in manifest makes it a
// workspace package so local builds resolve). napi must never write into the
// package dir directly: its output reconciliation deletes files it does not
// manage, including package.json. Release CI copies the same files from the
// prebuilt artifacts instead of rebuilding (see scripts/prepare-packages.mjs).
import { cpSync, mkdirSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const staging = join(root, 'wasm')
const pkgDir = join(root, 'npm/wasm')

rmSync(staging, { recursive: true, force: true })
mkdirSync(staging, { recursive: true })

execSync(
  'pnpm exec napi build --platform --release --target wasm32-wasip1'
  + ' --manifest-path native/Cargo.toml'
  + ' --output-dir wasm'
  // required for WASI builds in a `"type": "module"` package: the CommonJS
  // declaration must carry the .d.cts extension the manifest points at
  + ' --dts binding.wasip1.d.cts',
  { cwd: root, stdio: 'inherit' },
)

// the generic index/browser bindings and the debug-profile .wasm duplicate
// are not part of the package
const artifacts = readdirSync(staging).filter(
  file => file.startsWith('binding.') && !file.endsWith('.debug.wasm'),
)
if (artifacts.length === 0) {
  throw new Error('the wasm build produced no binding.* artifacts')
}
for (const file of artifacts) {
  cpSync(join(staging, file), join(pkgDir, file))
}

// the generated loader prefers a `.debug.wasm` over the release module
// whenever one sits next to it, so a debug artifact left by an earlier
// direct `napi build` would silently shadow every freshly built release
rmSync(join(pkgDir, 'binding.wasm32-wasip1.debug.wasm'), { force: true })

// napi skips the raw-module declaration when an explicit --dts path is set;
// the ./wasm export needs it. Bundler asset imports resolve to the module's
// URL string, so that is the declared contract.
writeFileSync(
  join(pkgDir, 'binding.wasm32-wasip1.wasm.d.ts'),
  'declare const wasmUrl: string;\nexport default wasmUrl;\n',
)
