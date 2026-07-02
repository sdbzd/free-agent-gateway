import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  estimatedTokens,
  heatLevel,
  normalizeUsageRows,
  percentText,
  reportedTokens,
  summarizeDaily,
  totalTokens,
} from "../src/usage.js";

describe("usage analytics helpers", () => {
  it("counts total tokens as prompt plus completion", () => {
    assert.equal(totalTokens({ total_prompt_tokens: 12, total_completion_tokens: 8 }), 20);
  });

  it("splits provider-reported tokens from locally estimated tokens", () => {
    const row = {
      reported_prompt_tokens: 10,
      reported_completion_tokens: 4,
      estimated_prompt_tokens: 6,
      estimated_completion_tokens: 2,
    };

    assert.equal(reportedTokens(row), 14);
    assert.equal(estimatedTokens(row), 8);
  });

  it("summarizes active days and streaks from dense daily buckets", () => {
    const summary = summarizeDaily([
      {
        total_requests: 1,
        total_prompt_tokens: 10,
        total_completion_tokens: 5,
        reported_prompt_tokens: 10,
        reported_completion_tokens: 5,
      },
      { total_requests: 0, total_prompt_tokens: 0, total_completion_tokens: 0 },
      {
        total_requests: 2,
        total_prompt_tokens: 20,
        total_completion_tokens: 10,
        estimated_prompt_tokens: 20,
        estimated_completion_tokens: 10,
      },
      {
        total_requests: 1,
        total_prompt_tokens: 4,
        total_completion_tokens: 1,
        reported_prompt_tokens: 4,
        reported_completion_tokens: 1,
      },
    ]);
    assert.deepEqual(summary, {
      active: 3,
      current: 2,
      estimated: 30,
      longest: 2,
      reported: 20,
      total: 50,
    });
  });

  it("sorts model rows by reported tokens, then request count", () => {
    const rows = normalizeUsageRows([
      { model_id: "small", total_prompt_tokens: 10, total_completion_tokens: 0, total_requests: 9 },
      { model_id: "large", total_prompt_tokens: 20, total_completion_tokens: 5, total_requests: 1 },
      { model_id: "tie", total_prompt_tokens: 10, total_completion_tokens: 0, total_requests: 10 },
    ]);
    assert.deepEqual(rows.map((row) => row.model_id), ["large", "tie", "small"]);
  });

  it("formats null coverage separately from zero coverage", () => {
    assert.equal(percentText(null), "--");
    assert.equal(percentText(0), "0%");
    assert.equal(percentText(0.607), "61%");
  });

  it("maps heat intensity into five non-empty levels", () => {
    assert.equal(heatLevel(0, 100), 0);
    assert.equal(heatLevel(5, 100), 1);
    assert.equal(heatLevel(20, 100), 2);
    assert.equal(heatLevel(40, 100), 3);
    assert.equal(heatLevel(70, 100), 4);
    assert.equal(heatLevel(90, 100), 5);
  });
});
