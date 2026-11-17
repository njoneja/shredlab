# app/tests

Unit tests for the UI's pure TypeScript modules in `app/src/lib`, run with
Node's built-in test runner. Node (24 or later) strips the types itself, so
there is no test framework or build step.

## Boundaries

- Only modules without DOM, Svelte or Tauri imports can be tested here.
  Type-only imports are fine (`import type`, which Node erases).
- The directory is outside `app/src`, so `svelte-check` and the Vite build
  ignore it. Test files import the module under test with its `.ts`
  extension, which only Node needs.

## Layout

One `<module>.test.ts` per module in `app/src/lib`.

## Running

```
pnpm --dir app test
```

The root `pnpm test` runs it too.

## Common changes

- New module test: add `tests/<module>.test.ts`, import from
  `../src/lib/<module>.ts`, and use `node:test` and `node:assert/strict`.
  If the module imports values (not only types) from another module
  without a `.ts` extension, Node can't resolve it; keep tested modules
  free of such imports.
