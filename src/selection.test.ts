import { describe, expect, it } from "vitest";
import { emptySelection, isSelected, normalizeFilter, selectSearch, toggleValue } from "./selection";
import type { Candidate } from "./types";

const candidate = (text: string): Candidate => ({ label: text, empty: !text, value: { kind: "text", text } });
describe("filter menu draft selection", () => {
  it("represents global selection with exclusions without enumerating the universe", () => {
    const s = toggleValue(emptySelection(), candidate("9"));
    expect(isSelected(s, candidate("1"))).toBe(true);
    expect(isSelected(s, candidate("9"))).toBe(false);
    expect(s.overrides).toHaveLength(1);
  });
  it("keeps point selections across search and selects all search matches", () => {
    let s = { ...emptySelection(), defaultSelected: false };
    s = toggleValue(s, candidate("小区1"));
    s = selectSearch(s, "QCI");
    expect(isSelected(s, candidate("小区1"))).toBe(true);
    expect(isSelected(s, candidate("QCI-999999"))).toBe(true);
    expect(isSelected(s, candidate("其他"))).toBe(false);
  });
  it("makes the latest bulk action override previous explicit exclusions", () => {
    const excluded = toggleValue(emptySelection(), candidate("alpha"));
    const allSearch = selectSearch(excluded, "alp");
    expect(isSelected(allSearch, candidate("alpha"))).toBe(true);
  });
  it("distinguishes select-none from clearing a filter", () => {
    expect(normalizeFilter({ selection: emptySelection(), predicates: [] })).toBeNull();
    expect(normalizeFilter({ selection: { ...emptySelection(), defaultSelected: false }, predicates: [] })).not.toBeNull();
  });
  it("matches full text instead of a truncated candidate label", () => {
    const item: Candidate = { label: "prefix…", value: { kind: "text", text: "prefix-long-tail" }, empty: false };
    const selection = selectSearch({ ...emptySelection(), defaultSelected: false }, "tail");
    expect(isSelected(selection, item)).toBe(true);
  });
  it("lets a later search rule override an older referenced value exclusion", () => {
    const item: Candidate = { label: "long…", value: { kind: "reference", rid: "1", column: 0 }, empty: false, matchingSearches: ["tail"] };
    const excluded = toggleValue(emptySelection(), item);
    const searched = selectSearch(excluded, "tail");
    expect(isSelected(searched, item)).toBe(true);
    expect(isSelected(toggleValue(searched, item), item)).toBe(false);
  });
  it("keeps a long value selected when another context uses a different representative record", () => {
    const first: Candidate = { label: "long…", value: { kind: "reference", rid: "1", column: 0, digest: "same-content", byteLength: 5000 }, empty: false };
    const next: Candidate = { ...first, value: { ...first.value as Extract<Candidate["value"], { kind: "reference" }>, rid: "9" } };
    const selected = toggleValue({ ...emptySelection(), defaultSelected: false }, first);
    expect(isSelected(selected, next)).toBe(true);
  });
});
