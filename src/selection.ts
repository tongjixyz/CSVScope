import type { Candidate, ColumnFilter, FilterValue, ValueSelection } from "./types";

export const emptySelection = (): ValueSelection => ({ defaultSelected: true, rules: [], overrides: [] });
export const valueKey = (v: FilterValue) => v.kind === "text" ? `text:${v.text}`
  : v.digest ? `ref:${v.column}:${v.byteLength}:${v.digest}` : `ref:${v.column}:${v.rid}`;
export function isSelected(selection: ValueSelection, candidate: Candidate): boolean {
  let selected = selection.defaultSelected;
  for (let stage = 0; stage <= selection.rules.length; stage++) {
    for (const point of selection.overrides) {
      if ((point.afterRule ?? 0) === stage && valueKey(point.value) === valueKey(candidate.value)) selected = point.selected;
    }
    const rule = selection.rules[stage];
    if (rule && (candidate.value.kind === "text" ? candidate.value.text.includes(rule.search)
      : candidate.matchingSearches?.includes(rule.search))) selected = rule.selected;
  }
  return selected;
}
export function toggleValue(selection: ValueSelection, candidate: Candidate): ValueSelection {
  const overrides = selection.overrides.filter(p => valueKey(p.value) !== valueKey(candidate.value));
  overrides.push({ value: candidate.value, selected: !isSelected(selection, candidate), afterRule: selection.rules.length });
  return { ...selection, overrides };
}
export function selectSearch(selection: ValueSelection, search: string): ValueSelection {
  return {
    ...selection,
    rules: [...selection.rules, { search, selected: true }],
    overrides: selection.overrides.filter(p => p.value.kind !== "text" || !p.value.text.includes(search)),
  };
}
export function filterSummary(filter: ColumnFilter): string {
  const parts: string[] = [];
  const s = filter.selection;
  if (s) {
    if (!s.defaultSelected && !s.rules.length && !s.overrides.some(p => p.selected)) parts.push("未选择任何值");
    else if (s.rules.length) parts.push(`${s.rules.length} 条选择规则`);
    else if (s.defaultSelected && s.overrides.length) parts.push(`排除 ${s.overrides.filter(p => !p.selected).length} 个值`);
    else if (!s.defaultSelected) parts.push(`选择 ${s.overrides.filter(p => p.selected).length} 个值`);
  }
  const names: Record<string, string> = {
    equals: "等于", notEquals: "不等于", contains: "包含", notContains: "不包含",
    empty: "空字段", notEmpty: "非空字段", numericRange: "数值范围", dateTimeRange: "时间范围",
  };
  for (const p of filter.predicates) parts.push(`${names[p.kind]}${"text" in p ? ` ${p.text}` : ""}`);
  return parts.join(" · ") || "全部值";
}
export function normalizeFilter(filter: ColumnFilter): ColumnFilter | null {
  const s = filter.selection;
  const unrestricted = !s || (s.defaultSelected && !s.rules.length && !s.overrides.length);
  return unrestricted && !filter.predicates.length ? null : filter;
}
export function formatBytes(raw: string | number): string {
  const n = Number(raw);
  if (!Number.isFinite(n) || n < 1024) return `${n || 0} B`;
  const exponent = Math.min(3, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** exponent).toFixed(exponent === 1 ? 0 : 1)} ${["B", "KB", "MB", "GB"][exponent]}`;
}
