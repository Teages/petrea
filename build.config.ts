import { defineBuildConfig } from 'obuild/config'

export default defineBuildConfig({
  entries: [
    { type: 'bundle', input: './src/index.ts', outDir: './dist' },
    // petrea/register and its threaded-hooks companion; the latter is loaded
    // by URL at runtime, so it must ship as its own file next to register.mjs
    { type: 'bundle', input: './src/register.ts', outDir: './dist' },
    { type: 'bundle', input: './src/register-hooks.ts', outDir: './dist' },
    {
      type: 'bundle',
      input: './src/wasm.ts',
      outDir: './dist',
      rolldown: {
        // the default `node` platform pulls node builtins into the bundle,
        // which browsers cannot load
        platform: 'browser',
        // the wasm binding ships as the @petrea/wasm package; keep the bare
        // import so consumers resolve it (and its wasm asset) from node_modules
        external: ['@petrea/wasm'],
      },
    },
  ],
})
