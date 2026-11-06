import { describe, expect, it } from "vitest";
import { b64ToBytes, budgetUsage, duration, FAILURE_LABEL, failureTag, fmtTokens, limitChip, minToSecs, numOrNull, providerLabel, retryHint, secsToMin, taskLimits, type Limit } from "./api";

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
  paused_reason: null,
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

describe("failure classes and limits", () => {
  it("chip shows an auth pause before anything else", () => {
    const c = limitChip(limit({ paused_reason: "Invalid API key", limited_until: "2026-01-01T10:00:00Z" }));
    expect(c).toEqual({ text: "Claude ⏸ pausiert (Anmeldung)", tone: "err" });
  });

  it("tags only non-task failures of unfinished tasks", () => {
    expect(failureTag({ failure_class: null, status: "queued" })).toBeNull();
    expect(failureTag({ failure_class: "task", status: "failed" })).toBeNull();
    expect(failureTag({ failure_class: "cancelled", status: "cancelled" })).toBeNull();
    expect(failureTag({ failure_class: "provider_transient", status: "done" })).toBeNull();
    expect(failureTag({ failure_class: "provider_transient", status: "queued" })).toEqual({ text: "Provider gestört", tone: "warn" });
    expect(failureTag({ failure_class: "limit_tokens", status: "blocked" })).toEqual({ text: "Limit: Tokens", tone: "err" });
    expect(FAILURE_LABEL.stalled).toBe("Stillstand");
  });

  it("retry hint only while the backoff is pending", () => {
    const now = new Date("2026-01-01T10:00:00Z");
    const t = { status: "queued" as const, not_before: "2026-01-01T10:00:30Z", transient_retries: 2 };
    expect(retryHint(t, now)).toMatch(/^wartet bis .+ \(Wiederholung 2\)$/);
    expect(retryHint({ ...t, not_before: "2026-01-01T09:59:59Z" }, now)).toBeNull();
    expect(retryHint({ ...t, status: "running" }, now)).toBeNull();
    expect(retryHint({ ...t, not_before: null }, now)).toBeNull();
    expect(retryHint({ ...t, transient_retries: 0 }, now)).not.toContain("Wiederholung");
  });

  it("converts minutes and numbers from inputs", () => {
    expect(secsToMin(null)).toBe("");
    expect(secsToMin(90)).toBe("1.5");
    expect(secsToMin(0)).toBe("0");
    expect(minToSecs("")).toBeNull();
    expect(minToSecs("1,5")).toBe(90);
    expect(minToSecs("0")).toBe(0);
    expect(() => minToSecs("-1")).toThrow();
    expect(() => minToSecs("abc")).toThrow();
    expect(numOrNull(" 2000 ")).toBe(2000);
    expect(numOrNull("0,25")).toBe(0.25);
    expect(numOrNull("")).toBeNull();
    expect(() => numOrNull("1e")).toThrow();
  });

  it("lists task limits", () => {
    expect(taskLimits({ max_runtime_secs: 120, max_cost_usd: 0.5, max_tokens: 20000, max_turns: 3 })).toEqual(["Laufzeit 2 Min.", "Kosten $0.5", "Tokens 20k", "Turns 3"]);
    expect(taskLimits({ max_runtime_secs: null, max_cost_usd: null, max_tokens: null, max_turns: null })).toEqual([]);
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
