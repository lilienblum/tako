import { describe, expect, test } from "bun:test";
import { defineWorkflow, isWorkflowDefinition, WORKFLOW_SYMBOL } from "../../src/workflows/define";

describe("WORKFLOW_SYMBOL", () => {
  test("is a symbol with description 'workflow'", () => {
    expect(typeof WORKFLOW_SYMBOL).toBe("symbol");
    expect(WORKFLOW_SYMBOL.description).toBe("workflow");
  });
});

describe("defineWorkflow", () => {
  test("returns an object with type, handler, and config", () => {
    const fn = async () => {};
    const def = defineWorkflow(fn, { schedule: "0 9 * * *" });
    expect(def.type).toBe(WORKFLOW_SYMBOL);
    expect(def.handler).toBe(fn);
    expect(def.config).toEqual({ schedule: "0 9 * * *" });
  });

  test("config defaults to empty object when not provided", () => {
    const fn = async () => {};
    const def = defineWorkflow(fn);
    expect(def.config).toEqual({});
  });

  test("does not mutate the original function", () => {
    const fn = async () => {};
    const originalKeys = Object.getOwnPropertySymbols(fn);
    defineWorkflow(fn);
    expect(Object.getOwnPropertySymbols(fn)).toEqual(originalKeys);
  });
});

describe("isWorkflowDefinition", () => {
  test("returns true for a defineWorkflow result", () => {
    const def = defineWorkflow(async () => {});
    expect(isWorkflowDefinition(def)).toBe(true);
  });

  test("returns false for a plain function", () => {
    expect(isWorkflowDefinition(async () => {})).toBe(false);
  });

  test("returns false for null", () => {
    expect(isWorkflowDefinition(null)).toBe(false);
  });

  test("returns false for a plain object without type", () => {
    expect(isWorkflowDefinition({ handler: () => {}, config: {} })).toBe(false);
  });

  test("returns false for a plain object with wrong type value", () => {
    expect(isWorkflowDefinition({ type: Symbol("workflow"), handler: () => {}, config: {} })).toBe(
      false,
    );
  });
});
