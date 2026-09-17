# petrea

A small, fast type-stripper that blanks TypeScript-only syntax using the [oxc parser](https://oxc.rs), leaving valid JavaScript with identical line and column positions.

## Install

```bash
npm install petrea
```

For browsers or platforms without a native binding, also install `@petrea/wasm` and import from `petrea/wasm`.

## Getting Started

```ts
import { transpile } from 'petrea'

console.log(await transpile(`const a: number = 1`))
// result: `const a         = 1`
```

`transpileSync` offers a synchronous alternative to `transpile`.

```ts
import { transpileSync } from 'petrea'

console.log(transpileSync(`const a: number = 1`))
// result: `const a         = 1`
```

## Run TypeScript directly in Node

```bash
node --import petrea/register ./app.ts
```

`petrea/register` hooks petrea into Node's module system, stripping types from
`.ts`/`.tsx`/`.mts`/`.cts` files as they load — both `import` and `require()`.
Module format follows the nearest `package.json` `type` field.

## License

[MIT](./LICENSE) — © 2025-present Teages.

Published binaries embed [oxc](https://github.com/oxc-project/oxc) and
[napi-rs](https://napi.rs) (both MIT); bundled JavaScript dependencies are
listed in `dist/THIRD-PARTY-LICENSES.md` at build time.

The test suite's fixture corpus in `test/fixture` is derived from
[ts-blank-space](https://github.com/bloomberg/ts-blank-space)
(© 2024 Bloomberg Finance L.P.) and remains under the
[Apache License 2.0](./test/fixture/LICENSE).
