export interface ParseSpec {
  encoding: string; delimiter: string | null; hasHeaders: boolean;
  maxRecordBytes: number; maxFieldBytes: number; maxColumns: number;
}
export const defaultParseSpec: ParseSpec = {
  encoding: "auto", delimiter: null, hasHeaders: true,
  maxRecordBytes: 16 * 1024 * 1024, maxFieldBytes: 8 * 1024 * 1024, maxColumns: 2048,
};
export interface Column { id: number; name: string; originalName: string }
export type FilterValue = { kind: "text"; text: string } | { kind: "reference"; rid: string; column: number; digest?: string; byteLength?: number };
export interface ValueSelection {
  defaultSelected: boolean;
  rules: { search: string; selected: boolean }[];
  overrides: { value: FilterValue; selected: boolean; afterRule?: number }[];
}
export type Predicate =
  | { kind: "equals" | "notEquals" | "contains" | "notContains"; text: string }
  | { kind: "empty" | "notEmpty" }
  | { kind: "numericRange" | "dateTimeRange"; min: string | null; max: string | null };
export interface ColumnFilter { selection: ValueSelection | null; predicates: Predicate[] }
export interface SortKey { column: number; descending: boolean; interpretation: "text" | "number" | "datetime" }
export interface ViewState { filters: Record<string, ColumnFilter>; sort: SortKey[] }
export interface QuerySnapshot {
  id: string; revision: number; status: string; generatedRows: string;
  exactTotal: string | null; projection: number[]; error: string | null;
}
export interface Session {
  id: string; path: string; name: string; status: string; phase: string;
  bytesRead: string; totalBytes: string; rowsRead: string; columns: Column[];
  preview: string[][]; parseSpec: ParseSpec; warnings: string[]; error: string | null;
  revision: number; view: ViewState; canUndo: boolean; canRedo: boolean;
  query: QuerySnapshot | null; cacheHit: boolean;
}
export interface Candidate { value: FilterValue; label: string; empty: boolean; matchingSearches?: string[] }
export interface CandidatePage {
  id: string; revision: number; items: Candidate[]; next: string | null;
  exactTotal: string | null; status: string;
}
export interface CandidateStatus {
  id: string; tabId: string; revision: number; column: number; search: string;
  status: string; count: string; error: string | null;
  availability: { value: FilterValue; available: boolean | null }[];
}
export interface ExportStatus {
  id: string; tabId: string; status: string; path: string;
  rowsWritten: string; totalRows: string; error: string | null;
}
export interface Layout {
  hidden: number[]; widths: Record<number, number>; frozen: number; start: number;
  scrollRow: number; scrollColumn: number;
}
export interface CacheUsage {
  path: string; datasets: string; sessions: string; limit: string;
  results: string; temporary: string; imports: string;
}
