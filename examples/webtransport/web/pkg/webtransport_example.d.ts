/* tslint:disable */
/* eslint-disable */

export function run_game(url: string, cert_hash_base64: string): void;

export function start(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly run_game: (a: number, b: number, c: number, d: number) => void;
    readonly start: () => void;
    readonly wasm_bindgen__closure__destroy__h0310da4081177587: (a: number, b: number) => void;
    readonly wasm_bindgen__closure__destroy__hebe8b1d377182928: (a: number, b: number) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h6c391c43eb52a6b0: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h11e87122f3f03f9e: (a: number, b: number) => number;
    readonly wasm_bindgen__convert__closures_____invoke__h1edc07f645a2cf88: (a: number, b: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
