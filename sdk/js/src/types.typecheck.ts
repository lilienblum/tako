import type { TakoOptions } from "./types";

const _emptyOptions: TakoOptions = {};

const _unsupportedReloadOption: TakoOptions = {
  // @ts-expect-error Removed with management-socket protocol cleanup.
  onConfigReload: () => {},
};
