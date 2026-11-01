import { describe, expect, it } from "vitest";
import { b64ToBytes, budgetUsage, duration, fmtTokens, limitChip, providerLabel, type Limit } from "./api";

const limit = (p: Partial<Limit> = {}): Limit => ({
  provider: "claude",
  used_5h: 0,
  used_7d: 0,
  cost_30d: 0,
  cache_read_5h: 0,
  runs_5h: 0,
  limit_5h_tokens: null,
  limit_7d_tokens: null,
  limit_30d_usd: null,
  limited_until: null,
  budget_until: null,
  budget_reason: null,
  observed_limit_tokens: null,
  observed_at: null,
  blocked: false,
  ...p,
});

describe("fmtTokens", () => {
  it("formats magnitudes", () => {
    expect(fmtTokens(999)).toBe("999");
    expect(fmtTokens(12_400)).toBe("12k");
    expect(fmtTokens(3_250_000)).toBe("3.3M");
  });
});

describe("duration", () => {
  it("formats seconds, minutes and hours", () => {
    const a = "2026-01-01T00:00:00Z";
    expect(duration(a, "2026-01-01T00:00:42Z")).toBe("42s");
    expect(duration(a, "2026-01-01T00:02:05Z")).toBe("2m 5s");
    expect(duration(a, "2026-01-01T01:30:00Z")).toBe("1h 30m");
    expect(duration(null, null)).toBe("");
  });
});

describe("budgets", () => {
  it("is null without budgets and uses the most exhausted window", () => {
    expect(budgetUsage(limit({ used_5h: 100 }))).toBeNull();
    expect(budgetUsage(limit({ used_5h: 50, limit_5h_tokens: 100, used_7d: 900, limit_7d_tokens: 1000 }))).toBeCloseTo(0.9);
    expect(budgetUsage(limit({ cost_30d: 15, limit_30d_usd: 10 }))).toBeCloseTo(1.5);
  });

  it("chip reflects subscription limit, budget pause and warning level", () => {
    expect(limitChip(limit({ limited_until: "2026-01-01T10:00:00Z" })).tone).toBe("err");
    expect(limitChip(limit({ budget_until: "2026-01-01T10:00:00Z" })).text).toContain("Budget");
    expect(limitChip(limit({ used_5h: 85, limit_5h_tokens: 100 }))).toEqual({ text: "Claude 85 %", tone: "warn" });
    expect(limitChip(limit({ used_5h: 10, limit_5h_tokens: 100 })).tone).toBe("");
    expect(limitChip(limit({ provider: "codex", used_5h: 2000 })).text).toBe("Codex (GPT) 2k");
  });
});

describe("misc", () => {
  it("labels providers", () => {
    expect(providerLabel("gemini")).toBe("Gemini");
    expect(providerLabel("x")).toBe("x");
  });
  it("decodes base64url VAPID keys", () => {
    expect(Array.from(b64ToBytes("AQID_-8"))).toEqual([1, 2, 3, 255, 239]);
  });
});
