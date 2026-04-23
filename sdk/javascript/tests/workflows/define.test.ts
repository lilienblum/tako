import { describe, expect, test } from "bun:test";
import {
  defineWorkflow,
  isWorkflowDefinition,
  isWorkflowExport,
  WORKFLOW_SYMBOL,
} from "../../src/workflows/define";

describe("WORKFLOW_SYMBOL", () => {
  test("is not equal to a separately created Symbol with the same description", () => {
    expect(Symbol("workflow")).not.toBe(WORKFLOW_SYMBOL);
  });
});

describe("defineWorkflow", () => {
  test("returns an export with enqueue + definition", () => {
    const fn = async () => {};
    const exp = defineWorkflow("my-job", fn, { schedule: "0 9 * * *" });
    expect(exp.definition.type).toBe(WORKFLOW_SYMBOL);
    expect(exp.definition.name).toBe("my-job");
    expect(exp.definition.handler).toBe(fn);
    expect(exp.definition.config).toEqual({ schedule: "0 9 * * *" });
    expect(typeof exp.enqueue).toBe("function");
  });

  test("config defaults to empty object when not provided", () => {
    const fn = async () => {};
    const exp = defineWorkflow("my-job", fn);
    expect(exp.definition.config).toEqual({});
  });
});

describe("isWorkflowExport", () => {
  test("returns true for a defineWorkflow result", () => {
    const exp = defineWorkflow("j", async () => {});
    expect(isWorkflowExport(exp)).toBe(true);
  });

  test("returns false for a plain function", () => {
    expect(isWorkflowExport(async () => {})).toBe(false);
  });

  test("returns false for null", () => {
    expect(isWorkflowExport(null)).toBe(false);
  });
});

describe("isWorkflowDefinition", () => {
  test("returns true for the inner definition of a defineWorkflow result", () => {
    const exp = defineWorkflow("j", async () => {});
    expect(isWorkflowDefinition(exp.definition)).toBe(true);
  });

  test("returns false for a plain object with wrong type value", () => {
    expect(
      isWorkflowDefinition({
        type: Symbol("workflow"),
        name: "x",
        handler: () => {},
        config: {},
      }),
    ).toBe(false);
  });
});
