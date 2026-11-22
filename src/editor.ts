import type { GridSelection, Rectangle } from "@glideapps/glide-data-grid";
import type { EditState, ViewState } from "./types";

export const emptyEdits = (): EditState => ({ cells: {}, deletedRows: [], deletedColumns: [], colors: {} });
export const editState = (view: ViewState): EditState => view.edits ?? emptyEdits();
export const MARK_COLORS = [
  { name: "黄色", value: "#fff1b8" }, { name: "绿色", value: "#d9f0de" },
  { name: "蓝色", value: "#d9eafa" }, { name: "粉色", value: "#f9dce6" },
  { name: "橙色", value: "#ffe1c2" }, { name: "紫色", value: "#e8ddf7" },
];

export function selectedColumns(selection: GridSelection | undefined, width: number): number[] {
  if (!selection) return [];
  const columns = new Set<number>();
  for (const index of selection.columns) if (index > 0 && index <= width) columns.add(index);
  if (!columns.size && selection.current) {
    for (const range of [...selection.current.rangeStack, selection.current.range]) {
      for (let index = Math.max(1, range.x); index < Math.min(width + 1, range.x + range.width); index++) columns.add(index);
    }
  }
  return [...columns].sort((a, b) => a - b);
}

/** Result positions are absolute; column positions include the source-record gutter. */
export function selectedRanges(selection: GridSelection | undefined, width: number, rows: number, start: number, total: number): Rectangle[] {
  if (!selection) return [];
  if (selection.columns.length) return selectedColumns(selection, width).map(x => ({ x, y: 0, width: 1, height: total }));
  if (selection.rows.length) return [...selection.rows].filter(y => y >= 0 && y < rows)
    .map(y => ({ x: 1, y: y + start, width, height: 1 }));
  if (!selection.current) return [];
  return [...selection.current.rangeStack, selection.current.range].flatMap(range => {
    const x = Math.max(1, range.x);
    const y = Math.max(0, range.y);
    const endX = Math.min(width + 1, range.x + range.width);
    const endY = Math.min(rows, range.y + range.height);
    return endX > x && endY > y ? [{ x, y: y + start, width: endX - x, height: endY - y }] : [];
  });
}

export function rowRanges(ranges: Rectangle[]): { start: number; count: number }[] {
  const intervals = ranges.map(r => [r.y, r.y + r.height]).sort((a, b) => a[0] - b[0]);
  const merged: number[][] = [];
  for (const [start, end] of intervals) {
    const last = merged.at(-1);
    if (last && start <= last[1]) last[1] = Math.max(last[1], end);
    else merged.push([start, end]);
  }
  return merged.map(([start, end]) => ({ start, count: end - start }));
}

export function quoteClipboard(rows: string[][]): string {
  return rows.map(row => row.map(value => /[\t\r\n"]/.test(value) ? `"${value.replaceAll('"', '""')}"` : value).join("\t")).join("\r\n");
}
