import { CompactSelection, type GridSelection } from "@glideapps/glide-data-grid";
import { describe, expect, it } from "vitest";
import { quoteClipboard, rowRanges, selectedColumns, selectedRanges } from "./editor";

const empty = (): GridSelection => ({ columns: CompactSelection.empty(), rows: CompactSelection.empty() });
describe("editing selections", () => {
  it("excludes the source-record gutter from editable columns and clips the last column", () => {
    const selection = { ...empty(), current: { cell: [0, 0] as const, range: { x: 0, y: 0, width: 8, height: 2 }, rangeStack: [] } };
    expect(selectedColumns(selection, 3)).toEqual([1, 2, 3]);
    expect(selectedRanges(selection, 3, 10, 500, 2000)).toEqual([{ x: 1, y: 500, width: 3, height: 2 }]);
  });
  it("maps selected rows in a moved grid window to absolute result positions", () => {
    const selection = { ...empty(), rows: CompactSelection.fromSingleSelection(2).add(7) };
    expect(selectedRanges(selection, 5, 10, 200_000, 500_000)).toEqual([
      { x: 1, y: 200_002, width: 5, height: 1 }, { x: 1, y: 200_007, width: 5, height: 1 },
    ]);
  });
  it("targets all result rows for a full-column selection and keeps nonadjacent columns separate", () => {
    const selection = { ...empty(), columns: CompactSelection.fromSingleSelection(1).add(3) };
    expect(selectedRanges(selection, 4, 200_000, 300_000, 900_000)).toEqual([
      { x: 1, y: 0, width: 1, height: 900_000 }, { x: 3, y: 0, width: 1, height: 900_000 },
    ]);
  });
  it("merges overlapping selection rectangles before resolving record identities", () => {
    expect(rowRanges([
      { x: 1, y: 4, width: 1, height: 3 }, { x: 2, y: 2, width: 2, height: 4 }, { x: 1, y: 9, width: 1, height: 1 },
    ])).toEqual([{ start: 2, count: 5 }, { start: 9, count: 1 }]);
  });
  it("quotes clipboard fields without changing long identifiers, tabs or multiline text", () => {
    expect(quoteClipboard([["00123456789012345678", 'a"\tb'], ["第一行\r\n第二行", ""]]))
      .toBe('00123456789012345678\t"a""\tb"\r\n"第一行\r\n第二行"\t');
  });
});
