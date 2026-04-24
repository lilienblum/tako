/**
 * Dynamic-import helper for server-side file discovery.
 *
 * We need a truly-dynamic `import(url)` in `workflows/discovery`,
 * `channels/discovery`, and `create-entrypoint` — they resolve user
 * files by path at runtime. Vite's `vite:import-analysis` plugin warns
 * whenever it sees `await import(<non-literal>)` in any module it
 * ingests, including bundled SDK chunks pulled in during dep
 * pre-bundling. The conventional suppressor is `/* @vite-ignore *\/`,
 * but tsdown's minifier strips those annotations, so the warning
 * re-surfaces in every downstream consumer.
 *
 * Going through a `Function`-constructor indirection means the bundled
 * output never contains a literal `import(...)` call — Vite's scanner
 * sees only a function invocation and never warns. Server-side only,
 * so the `unsafe-eval`-equivalent nature of `Function` has no CSP
 * implications here.
 */
export const dynImport = new Function("u", "return import(u)") as (u: string) => Promise<unknown>;
