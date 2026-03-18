import type { TakoOptions } from "./types";

// Compile-time type assertions — void suppresses noUnusedLocals.
const _emptyOptions: TakoOptions = {};
void _emptyOptions;

const _unsupportedReloadOption: TakoOptions = {
  // @ts-expect-error Removed with management-socket protocol cleanup.
  onConfigReload: () => {},
};
void _unsupportedReloadOption;
