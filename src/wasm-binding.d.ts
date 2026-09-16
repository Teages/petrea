// Ambient types for the @petrea/binding-wasm32-wasip1 loader, whose real
// declaration file (binding.wasip1.d.cts) only resolves once that package is
// installed. Declaring the shape here keeps `pnpm test:types` green in the
// repo, where the package exists as a workspace link whose generated types
// land only after `pnpm build:wasm`.
declare module '@petrea/binding-wasm32-wasip1' {
  export interface NativeOptions {
    lang?: string
    filename?: string
  }

  export interface NativeResult {
    code: string
    unsupported: unknown[]
  }

  export interface NativeUnitsResult {
    code: Uint16Array
    unsupported: unknown[]
  }

  export function transpileAsync(
    input: string,
    options?: NativeOptions,
  ): Promise<NativeResult>
  export function transpileNativeSync(
    input: string,
    options?: NativeOptions,
  ): NativeResult
  export function transpileUtf16Async(
    units: Uint16Array,
    options?: NativeOptions,
  ): Promise<NativeUnitsResult>
  export function transpileUtf16Sync(
    units: Uint16Array,
    options?: NativeOptions,
  ): NativeUnitsResult
}
