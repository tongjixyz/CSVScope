import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import DataEditor, { CompactSelection, GridCellKind, type DataEditorRef, type GridCell, type GridColumn, type GridSelection, type Item, type Rectangle } from "@glideapps/glide-data-grid";
import type { Table } from "apache-arrow";
import {
  ArrowDown, ArrowUp, Check, ChevronDown, Columns3, Copy, Database, Download, Eraser, Eye, EyeOff, FileSpreadsheet,
  FileText, Filter, FolderOpen, HardDrive, HelpCircle, LoaderCircle, Redo2, Settings2, ShieldCheck,
  Pencil, Save, Trash2, Undo2, X, ZoomIn,
} from "lucide-react";
import { api, chooseExport, chooseFiles, native } from "./api";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { reportFrontendError } from "./diagnostics";
import FilterMenu from "./FilterMenu";
import ContextMenu, { type MenuAction } from "./ContextMenu";
import { editState, quoteClipboard, rowRanges, selectedColumns, selectedRanges } from "./editor";
import { filterSummary, formatBytes } from "./selection";
import { defaultParseSpec, type CacheUsage, type ExportStatus, type Layout, type ParseSpec, type Session, type ViewState } from "./types";
import { version } from "../package.json";

const defaultLayout = (): Layout => ({ hidden: [], widths: {}, frozen: 0, start: 0, scrollRow: 0, scrollColumn: 0 });
const PAGE_SIZE = 256;
const WINDOW_SIZE = 200_000;
const emptyGridSelection: GridSelection = { columns: CompactSelection.empty(), rows: CompactSelection.empty() };
interface CellDraft {
  name: string; value: string; original: string; loading: boolean;
  tabId: string; revision: number; rid: string; column: number;
}

export default function App() {
  const [tabs, setTabs] = useState<Session[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [layouts, setLayouts] = useState<Record<string, Layout>>({});
  const [filterColumn, setFilterColumn] = useState<number | null>(null);
  const [columnsOpen, setColumnsOpen] = useState(false);
  const [parseOpen, setParseOpen] = useState(false);
  const [parseSpec, setParseSpec] = useState<ParseSpec>(defaultParseSpec);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [jump, setJump] = useState("");
  const [region, setRegion] = useState<Rectangle>({ x: 0, y: 0, width: 8, height: 35 });
  const [cell, setCell] = useState<CellDraft | null>(null);
  const [editBusy, setEditBusy] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; selection: GridSelection; item?: Item } | null>(null);
  const [closeTab, setCloseTab] = useState<Session | null>(null);
  const [closeApp, setCloseApp] = useState(false);
  const [savedCopies, setSavedCopies] = useState<Record<string, string>>({});
  const [exportDialog, setExportDialog] = useState(false);
  const [exportScope, setExportScope] = useState("all");
  const [exportRows, setExportRows] = useState("all");
  const [exportEncoding, setExportEncoding] = useState("utf-8");
  const [exportBom, setExportBom] = useState(false);
  const [exports, setExports] = useState<ExportStatus[]>([]);
  const [cache, setCache] = useState<CacheUsage | null>(null);
  const [selection, setSelection] = useState<GridSelection>();
  const selectionBeforePointer = useRef<GridSelection>();
  const grid = useRef<DataEditorRef>(null);
  const pages = useRef(new Map<string, Table>());
  const inflight = useRef(new Set<string>());
  const pagePump = useRef<() => void>(() => {});
  const mounted = useRef(true);
  const activeRef = useRef<string | null>(null);
  const queryRef = useRef<string | undefined>();
  const selections = useRef(new Map<string, GridSelection | undefined>());
  const seenRevision = useRef(new Map<string, number>());
  const changingWindow = useRef(false);
  const refreshRef = useRef<() => Promise<void>>(async () => {});
  const refreshTicket = useRef(0);
  const appliedRefresh = useRef(0);
  const refreshing = useRef(false);
  const current = tabs.find(t => t.id === active);
  const isUnsaved = (tab: Session) => tab.modified && savedCopies[tab.id] !== JSON.stringify(editState(tab.view));
  const unsavedTabs = useRef<Session[]>([]);
  unsavedTabs.current = tabs.filter(isUnsaved);
  const layout = active ? layouts[active] ?? defaultLayout() : defaultLayout();
  const liveColumns = useMemo(() => current?.columns.filter(c => !editState(current.view).deletedColumns.includes(c.id)) ?? [],
    [current?.columns, current?.view.edits?.deletedColumns]);
  const visible = useMemo(() => liveColumns.filter(c => !layout.hidden.includes(c.id)), [liveColumns, layout.hidden]);
  const nativeReady = current?.status === "ready";
  const query = current?.query?.revision === current?.revision ? current?.query : undefined;
  queryRef.current = query?.id;
  const generated = Number(query?.generatedRows ?? 0);
  const loadingRows = query?.status === "loading";
  const hasFilters = !!current && Object.keys(current.view.filters).length > 0;
  const previewMode = !query;
  const canRead = !!query && (nativeReady || loadingRows);
  const absoluteRows = previewMode ? current?.preview.length ?? 0 : Number(query?.exactTotal ?? generated);
  const windowStart = Math.min(layout.start, Math.max(0, absoluteRows - 1));
  const rows = Math.min(WINDOW_SIZE, Math.max(0, absoluteRows - windowStart));
  const projection = useMemo(() => {
    const frozenColumns = visible.slice(0, layout.frozen);
    const scrollableColumns = visible.slice(Math.max(layout.frozen, region.x - 3), region.x + region.width + 1);
    return [...frozenColumns, ...scrollableColumns].map(column => column.id);
  }, [visible, layout.frozen, region.x, region.width]);

  const fail = useCallback((e: unknown) => {
    reportFrontendError("frontend_action_failed", e);
    setError(String(e));
  }, []);
  const refresh = useCallback(async () => {
    if (!native || !mounted.current || refreshing.current) return;
    refreshing.current = true;
    const ticket = ++refreshTicket.current;
    try {
      const nextTabs = await api.sessions();
      if (!mounted.current || ticket < appliedRefresh.current) return;
      appliedRefresh.current = ticket;
      setTabs(previous => nextTabs.map(next => {
        const old = previous.find(t => t.id === next.id);
        if (!old) return next;
        if (old.revision > next.revision) return old;
        return {
          ...next,
          columns: JSON.stringify(old.columns) === JSON.stringify(next.columns) ? old.columns : next.columns,
          query: next.query && old.query ? {
            ...next.query,
            projection: old.query.projection.join(",") === next.query.projection.join(",") ? old.query.projection : next.query.projection,
          } : next.query,
        };
      }));
      const ids = new Set(nextTabs.map(t => t.id));
      setLayouts(old => Object.keys(old).every(id => ids.has(id)) ? old
        : Object.fromEntries(Object.entries(old).filter(([id]) => ids.has(id))));
      for (const id of selections.current.keys()) if (!ids.has(id)) selections.current.delete(id);
      for (const id of seenRevision.current.keys()) if (!ids.has(id)) seenRevision.current.delete(id);
      setActive(previous => previous && nextTabs.some(t => t.id === previous) ? previous : nextTabs[0]?.id ?? null);
    } catch (e) { fail(e); }
    finally { refreshing.current = false; }
  }, [fail]);
  refreshRef.current = refresh;
  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => { mounted.current = false; };
  }, [refresh]);
  useEffect(() => {
    if (!native) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void getCurrentWindow().onCloseRequested(event => {
      if (unsavedTabs.current.length) { event.preventDefault(); setCloseApp(true); }
    }).then(cleanup => { if (disposed) cleanup(); else unlisten = cleanup; }).catch(fail);
    return () => { disposed = true; unlisten?.(); };
  }, [fail]);
  const busy = tabs.some(t => ["opening", "previewing", "preparing", "cancelling"].includes(t.status)
    || (t.status === "ready" && (!t.query || t.query.revision !== t.revision)) || ["queued", "building"].includes(t.query?.status ?? ""));
  useEffect(() => {
    if (!native) return;
    const timer = setInterval(() => void refresh(), busy ? 350 : 1200);
    return () => clearInterval(timer);
  }, [busy, refresh]);
  useLayoutEffect(() => {
    activeRef.current = active;
    pages.current.clear(); setSelection(active ? selections.current.get(active) : undefined); setFilterColumn(null); setContextMenu(null); setCell(null); setColumnsOpen(false);
    const saved = active ? layouts[active] ?? defaultLayout() : defaultLayout();
    changingWindow.current = true;
    setRegion({ x: saved.scrollColumn, y: saved.scrollRow, width: 8, height: 35 });
    let settledFrame: number | undefined;
    const restoreFrame = requestAnimationFrame(() => {
      grid.current?.scrollTo(saved.scrollColumn, saved.scrollRow, "both", 0, 0, { vAlign: "start", hAlign: "start" });
      settledFrame = requestAnimationFrame(() => { changingWindow.current = false; });
    });
    if (active && native) void api.activate(active).then(refresh).catch(fail);
    return () => {
      cancelAnimationFrame(restoreFrame);
      if (settledFrame !== undefined) cancelAnimationFrame(settledFrame);
    };
  }, [active, refresh, fail]);
  useLayoutEffect(() => {
    pages.current.clear();
    if (current) {
      const previous = seenRevision.current.get(current.id);
      seenRevision.current.set(current.id, current.revision);
      if (previous !== undefined && previous !== current.revision) {
        setLayouts(old => ({ ...old, [current.id]: { ...(old[current.id] ?? defaultLayout()), start: 0, scrollRow: 0 } }));
        setSelection(undefined); selections.current.delete(current.id);
        grid.current?.scrollTo(0, 0, "vertical");
      }
    }
  }, [current?.id, current?.revision, query?.id]);
  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(""), 5000);
    return () => clearTimeout(timer);
  }, [notice]);

  const openFiles = useCallback(async () => {
    try {
      if (!native) { setNotice("请使用桌面版打开本地文件。界面开发：npm run desktop"); return; }
      const paths = await chooseFiles();
      for (const path of paths) {
        const id = await api.open(path, parseSpec);
        setActive(id);
      }
      await refresh();
    } catch (e) { fail(e); }
  }, [parseSpec, refresh, fail]);
  const commit = useCallback(async (view: ViewState) => {
    if (!current) return;
    await api.commit(current, view);
    await refresh();
  }, [current, refresh]);
  const history = useCallback(async (redo: boolean) => {
    if (!current) return;
    try { await api.history(current, redo); await refresh(); } catch (e) { fail(e); }
  }, [current, refresh, fail]);
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.closest("input, textarea, select, [contenteditable=true]")) return;
      if (e.ctrlKey && e.key.toLowerCase() === "o") { e.preventDefault(); void openFiles(); }
      if (e.ctrlKey && (e.key.toLowerCase() === "z" || e.key.toLowerCase() === "y")) {
        e.preventDefault();
        if (filterColumn === null) void history(e.key.toLowerCase() === "y" || e.shiftKey);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [openFiles, history, filterColumn]);

  useEffect(() => {
    if (!current || !query || !canRead || !projection.length) return;
    if (nativeReady && projection.some(c => !query.projection.includes(c))) {
      const timer = setTimeout(() => {
        void api.projection(current.id, current.revision, [...new Set([...query.projection, ...projection])].slice(-128)).then(refresh).catch(e => { if (!String(e).includes("Stale")) fail(e); });
      }, 200);
      return () => clearTimeout(timer);
    }
    const startPage = Math.floor((windowStart + region.y) / PAGE_SIZE);
    const endPage = Math.floor(Math.min(generated - 1, windowStart + region.y + region.height + PAGE_SIZE) / PAGE_SIZE);
    const gridColumns = [0, ...visible.flatMap((column, index) => projection.includes(column.id) ? [index + 1] : [])];
    let cancelled = false;
    let running = 0;
    const pending: number[] = [];
    for (let page = Math.max(0, startPage - 1); page <= endPage; page++) {
      const key = `${query.id}:${page}:${projection.join(",")}`;
      const cached = pages.current.get(key);
      if ((cached && cached.numRows >= Math.min(PAGE_SIZE, generated - page * PAGE_SIZE)) || inflight.current.has(key)) continue;
      pending.push(page);
    }
    const run = () => {
      if (cancelled) return;
      while (running < 2 && inflight.current.size < 2 && pending.length) {
        const page = pending.shift()!;
        const key = `${query.id}:${page}:${projection.join(",")}`;
        running++; inflight.current.add(key);
        void api.page(current.id, query.id, page * PAGE_SIZE, projection).then(table => {
          if (!mounted.current || activeRef.current !== current.id || queryRef.current !== query.id) return;
          pages.current.set(key, table);
          while (pages.current.size > 20) pages.current.delete(pages.current.keys().next().value!);
          grid.current?.updateCells(Array.from({ length: table.numRows }, (_, row) => page * PAGE_SIZE + row - windowStart)
            .filter(row => row >= 0 && row < WINDOW_SIZE)
            .flatMap(row => gridColumns.map(column => ({ cell: [column, row] as Item }))));
        }).catch(e => {
          if (!cancelled && !String(e).includes("Stale") && !String(e).includes("Pending")) fail(e);
        }).finally(() => { inflight.current.delete(key); running--; pagePump.current(); });
      }
    };
    pagePump.current = run;
    run();
    return () => { cancelled = true; if (pagePump.current === run) pagePump.current = () => {}; };
  }, [current?.id, query?.id, query?.status, query?.generatedRows, query?.projection, nativeReady, canRead, projection, region.y, region.height, windowStart, generated, refresh, fail, visible]);

  const getCell = useCallback(([columnIndex, row]: Item): GridCell => {
    const column = visible[columnIndex - 1];
    if (!current) return { kind: GridCellKind.Loading, allowOverlay: false };
    if (columnIndex === 0) {
      const absolute = row + windowStart;
      const table = pages.current.get(`${query?.id}:${Math.floor(absolute / PAGE_SIZE)}:${projection.join(",")}`);
      const rid = previewMode ? absolute + 1 : table?.getChild("_source_rid")?.get(absolute % PAGE_SIZE);
      return rid === undefined || rid === null ? { kind: GridCellKind.Loading, allowOverlay: false }
        : { kind: GridCellKind.Text, data: String(rid), displayData: String(rid), readonly: true, allowOverlay: false, themeOverride: { textDark: "#9aa6ae", bgCell: "#fafbfc" } };
    }
    if (!column) return { kind: GridCellKind.Loading, allowOverlay: false };
    if (previewMode) {
      const value = current.preview[row + windowStart]?.[column.id] ?? "";
      return { kind: GridCellKind.Text, data: value, displayData: value, readonly: true, allowOverlay: false };
    }
    const absolute = row + windowStart;
    const page = Math.floor(absolute / PAGE_SIZE);
    const key = `${query?.id}:${page}:${projection.join(",")}`;
    const table = pages.current.get(key);
    const value = table?.getChild(`c${column.id}`)?.get(absolute % PAGE_SIZE);
    if (value === undefined || value === null) return { kind: GridCellKind.Loading, allowOverlay: false };
    const text = String(value);
    const rid = table?.getChild("_source_rid")?.get(absolute % PAGE_SIZE);
    const color = rid == null ? undefined : current.view.edits?.colors[String(rid)]?.[column.id];
    return { kind: GridCellKind.Text, data: text, displayData: text, readonly: true, allowOverlay: false,
      themeOverride: color ? { bgCell: color, bgCellMedium: color } : undefined };
  }, [current, visible, previewMode, windowStart, query?.id, projection]);

  const columns: GridColumn[] = [{ id: "source-rid", title: "源记录", width: 84, hasMenu: false }, ...visible.map(c => ({
    id: String(c.id), title: c.name,
    width: layout.widths[c.id] ?? (c.name.length > 16 ? 220 : 170),
    hasMenu: true,
    themeOverride: current?.view.filters[c.id] ? { bgHeader: "#e2f1e9", textHeader: "#176b5b" } : undefined,
  }))];
  const activateCell = async ([columnIndex, row]: Item) => {
    const column = visible[columnIndex - 1];
    if (!current || !column) return;
    const revision = current.revision;
    const draft: CellDraft = { name: `${column.name} · 记录 ${row + windowStart + 1}`, value: "", original: "", loading: true,
      tabId: current.id, revision, rid: "", column: column.id };
    setCell(draft);
    try {
      let value: string;
      let sourceRid = String(row + windowStart + 1);
      if (previewMode) value = current.preview[row + windowStart]?.[column.id] ?? "";
      else {
        const absolute = row + windowStart;
        const table = pages.current.get(`${query?.id}:${Math.floor(absolute / PAGE_SIZE)}:${projection.join(",")}`);
        const rid = table?.getChild("_source_rid")?.get(absolute % PAGE_SIZE);
        if (rid === undefined || rid === null) throw new Error("单元格仍在加载");
        sourceRid = String(rid);
        value = await api.cell(current.id, String(rid), column.id);
      }
      if (activeRef.current === current.id) setCell({ ...draft, name: `${column.name} · 源记录 ${sourceRid}${previewMode ? "（预览）" : loadingRows ? "（后台加载中）" : ""}`,
        rid: sourceRid, value, original: value, loading: false });
    } catch (e) { setCell(null); fail(e); }
  };
  const changeLayout = (next: Partial<Layout>) => {
    if (!active) return;
    setLayouts(old => ({ ...old, [active]: { ...(old[active] ?? defaultLayout()), ...next } }));
  };
  const selectGrid = (next: GridSelection) => {
    setSelection(next);
    if (active) selections.current.set(active, next);
  };
  const clearSelection = () => {
    setSelection(undefined);
    if (active) selections.current.delete(active);
  };
  const canEdit = nativeReady && query?.status === "ready" && !editBusy;
  const rangesFor = (target: GridSelection | undefined) => selectedRanges(target, visible.length, rows, windowStart, absoluteRows);
  const sourceRows = async (ranges: Rectangle[]) => {
    if (!current || !query) throw new Error("完整结果就绪后才能编辑");
    const rangesToRead = rowRanges(ranges);
    if (rangesToRead.reduce((count, range) => count + range.count, 0) > 100_000) throw new Error("一次最多操作 100,000 行，请分批选择");
    const records = new Map<number, string>();
    for (const range of rangesToRead) {
      const data = await api.copy(current.id, query.id, range.start, range.count, [], true);
      if (data.length !== range.count) throw new Error("选区已变化，请重新选择");
      data.forEach((values, index) => records.set(range.start + index, values[0]));
    }
    return records;
  };
  const mutateSelection = async (operation: "color" | "clear" | "deleteRows" | "deleteColumns", target: GridSelection | undefined, color: string | null = null) => {
    if (!current || !canEdit) return;
    setEditBusy(true);
    try {
      const view = structuredClone(current.view);
      const edits = view.edits = structuredClone(editState(view));
      if (operation === "deleteColumns") {
        const ids = selectedColumns(target, visible.length).map(index => visible[index - 1].id);
        if (!ids.length) throw new Error("请先选择数据列");
        if (ids.length >= liveColumns.length) throw new Error("至少保留一列数据");
        edits.deletedColumns = [...new Set([...edits.deletedColumns, ...ids])];
        ids.forEach(id => delete view.filters[id]);
        view.sort = view.sort.filter(key => !ids.includes(key.column));
        await commit(view);
        const remaining = liveColumns.filter(column => !ids.includes(column.id));
        if (remaining.every(column => layout.hidden.includes(column.id))) changeLayout({ hidden: layout.hidden.filter(id => id !== remaining[0].id) });
        clearSelection();
        setNotice(`已删除 ${ids.length} 列，可撤销恢复`);
        return;
      }
      if (operation === "deleteRows" && target?.columns.length) throw new Error("请点击源记录号选择行，或框选单元格后删除所在行");
      const ranges = rangesFor(target);
      if (!ranges.length) throw new Error("请先选择单元格或行");
      if (operation !== "deleteRows" && ranges.reduce((count, range) => count + range.width * range.height, 0) > 100_000) {
        throw new Error("一次最多修改 100,000 个单元格，请缩小选区");
      }
      const records = await sourceRows(ranges);
      if (operation === "deleteRows") {
        edits.deletedRows = [...new Set([...edits.deletedRows, ...records.values()])];
      } else {
        for (const range of ranges) for (let row = range.y; row < range.y + range.height; row++) {
          const rid = records.get(row)!;
          for (let index = range.x; index < range.x + range.width; index++) {
            const column = visible[index - 1].id;
            if (operation === "clear") (edits.cells[rid] ??= {})[column] = "";
            else if (color) (edits.colors[rid] ??= {})[column] = color;
            else if (edits.colors[rid]) {
              delete edits.colors[rid][column];
              if (!Object.keys(edits.colors[rid]).length) delete edits.colors[rid];
            }
          }
        }
      }
      await commit(view);
      if (operation === "deleteRows") clearSelection();
      setNotice(operation === "deleteRows" ? `已删除 ${records.size} 行，可撤销恢复`
        : operation === "clear" ? "已清空选中内容，可撤销恢复" : color ? "已标记颜色" : "已清除颜色标记");
    } catch (e) { fail(e); }
    finally { setEditBusy(false); }
  };
  const hideSelectedColumns = (target: GridSelection | undefined) => {
    const ids = selectedColumns(target, visible.length).map(index => visible[index - 1].id);
    if (!ids.length) return;
    if (ids.length >= visible.length) { setNotice("至少保留一列可见数据"); return; }
    changeLayout({ hidden: [...new Set([...layout.hidden, ...ids])], frozen: Math.min(layout.frozen, visible.length - ids.length) });
    clearSelection();
    setNotice(`已隐藏 ${ids.length} 列，可在“列布局”中恢复`);
  };
  const copySelection = async (target: GridSelection | undefined) => {
    if (!current || !query) return;
    try {
      const ranges = rangesFor(target);
      if (ranges.reduce((count, range) => count + range.width * range.height, 0) > 100_000) throw new Error("一次最多复制 100,000 个单元格，请使用另存为 CSV");
      const data: string[][] = [];
      for (const range of ranges) data.push(...await api.copy(current.id, query.id, range.y, range.height,
        visible.slice(range.x - 1, range.x + range.width - 1).map(column => column.id), false));
      await navigator.clipboard.writeText(quoteClipboard(data));
      setNotice("已复制所选内容");
    } catch (e) { fail(e); }
  };
  const pasteCells = async ([columnIndex, row]: Item, values: readonly (readonly string[])[]) => {
    if (!current || !canEdit) { setNotice("请等待完整数据就绪后粘贴"); return; }
    setEditBusy(true);
    try {
      if (columnIndex < 1) throw new Error("源记录号不可编辑，请选择数据单元格");
      const width = Math.max(0, ...values.map(value => value.length));
      if (values.length * width > 100_000) throw new Error("一次最多粘贴 100,000 个单元格");
      if (columnIndex + width > visible.length + 1 || row + values.length > rows) throw new Error("粘贴范围超出当前数据行列");
      const records = await sourceRows([{ x: columnIndex, y: row + windowStart, width, height: values.length }]);
      const view = structuredClone(current.view);
      const edits = view.edits = structuredClone(editState(view));
      values.forEach((fields, offset) => {
        const rid = records.get(row + windowStart + offset)!;
        fields.forEach((value, index) => (edits.cells[rid] ??= {})[visible[columnIndex + index - 1].id] = value);
      });
      await commit(view);
      setNotice("已粘贴，可撤销恢复");
    } catch (e) { fail(e); }
    finally { setEditBusy(false); }
  };
  const saveCell = async () => {
    if (!cell || !current || !canEdit) return;
    if (cell.tabId !== current.id || cell.revision !== current.revision) { fail("单元格上下文已变化，请关闭后重新编辑"); return; }
    setEditBusy(true);
    try {
      const view = structuredClone(current.view);
      const edits = view.edits = structuredClone(editState(view));
      (edits.cells[cell.rid] ??= {})[cell.column] = cell.value;
      await commit(view);
      setCell(null);
      setNotice("单元格已修改，请另存为 CSV 保存");
    } catch (e) { fail(e); }
    finally { setEditBusy(false); }
  };
  const requestClose = (tab: Session) => {
    if (isUnsaved(tab)) setCloseTab(tab);
    else void api.close(tab.id).then(refresh).catch(fail);
  };
  const closeContextMenu = useCallback(() => setContextMenu(null), []);
  const showCellMenu = (item: Item, x: number, y: number) => {
    const [column, row] = item;
    const insideRange = selection?.current && [...selection.current.rangeStack, selection.current.range]
      .some(range => column >= range.x && column < range.x + range.width && row >= range.y && row < range.y + range.height);
    const retained = selection && (insideRange || selection.rows.hasIndex(row) || selection.columns.hasIndex(column));
    const target: GridSelection = retained ? selection! : column === 0
      ? { ...emptyGridSelection, rows: CompactSelection.fromSingleSelection(row) }
      : { ...emptyGridSelection, current: { cell: item, range: { x: column, y: row, width: 1, height: 1 }, rangeStack: [] } };
    selectGrid(target);
    setContextMenu({ x, y, selection: target, item });
  };
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.ctrlKey && ["r", "p"].includes(event.key.toLowerCase())) event.preventDefault();
      if (event.ctrlKey && event.key.toLowerCase() === "s") {
        event.preventDefault();
        if (query?.status === "ready" && !cell && filterColumn === null) setExportDialog(true);
      }
      const target = event.target as HTMLElement;
      if (event.key === "F2" && selection?.current && !target.closest("input, textarea, select, [contenteditable=true]") && !cell) {
        event.preventDefault(); void activateCell(selection.current.cell);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  });
  const visibleRegionChanged = (next: Rectangle) => {
    setRegion(next);
    if (!active || changingWindow.current) return;
    // Glide's finite window is moved before its end, preserving the absolute record.
    if (next.y > WINDOW_SIZE - 1000 && windowStart + WINDOW_SIZE < absoluteRows) {
      const advance = Math.floor(WINDOW_SIZE / 2);
      changingWindow.current = true;
      setSelection(undefined); selections.current.delete(active);
      changeLayout({ start: windowStart + advance, scrollRow: next.y - advance, scrollColumn: next.x });
      requestAnimationFrame(() => {
        grid.current?.scrollTo(next.x, next.y - advance, "vertical", 0, 0, { vAlign: "start" });
        requestAnimationFrame(() => { changingWindow.current = false; });
      });
    } else if (next.y < 100 && windowStart > 0) {
      const advance = Math.min(windowStart, Math.floor(WINDOW_SIZE / 2));
      changingWindow.current = true;
      setSelection(undefined); selections.current.delete(active);
      changeLayout({ start: windowStart - advance, scrollRow: next.y + advance, scrollColumn: next.x });
      requestAnimationFrame(() => {
        grid.current?.scrollTo(next.x, next.y + advance, "vertical", 0, 0, { vAlign: "start" });
        requestAnimationFrame(() => { changingWindow.current = false; });
      });
    } else {
      changeLayout({ scrollRow: next.y, scrollColumn: next.x });
    }
  };
  const gotoRow = () => {
    const position = Number(jump) - 1;
    if (!Number.isSafeInteger(position) || position < 0) { setNotice("请输入有效的正整数行号"); return; }
    if (position >= absoluteRows) {
      setNotice(query?.status === "ready" ? "行号超出结果范围" : loadingRows ? "该行尚未加载，后台仍在继续读取" : "该位置尚未生成，请等待完整结果");
      return;
    }
    const start = Math.max(0, position - Math.floor(WINDOW_SIZE / 2));
    changingWindow.current = true;
    setSelection(undefined); if (active) selections.current.delete(active);
    changeLayout({ start, scrollRow: position - start });
    requestAnimationFrame(() => {
      grid.current?.scrollTo(0, position - start, "vertical", 0, 0, { vAlign: "start" });
      requestAnimationFrame(() => { changingWindow.current = false; });
    });
  };
  const startExport = async () => {
    if (!current) return;
    try {
      const path = await chooseExport(current.name);
      if (!path) return;
      const exportId = await api.export(current, path, exportScope === "all" ? liveColumns.map(c => c.id) : visible.map(c => c.id), exportBom, exportEncoding, exportRows === "all");
      const exportedEdits = JSON.stringify(editState(current.view));
      const completeCopy = exportScope === "all" && exportRows === "all";
      const exportedTab = current.id;
      setExportDialog(false);
      const poll = async () => {
        try {
          const status = await api.exportStatus(exportId);
          if (!mounted.current) return;
          setExports(old => [...old.filter(e => e.id !== status.id), status]);
          if (["queued", "running"].includes(status.status)) setTimeout(() => void poll(), 400);
          else if (status.error) fail(status.error);
          else if (status.status === "completed") {
            if (completeCopy) setSavedCopies(old => ({ ...old, [exportedTab]: exportedEdits }));
            setNotice(`CSV 已保存：${status.path}（${Number(status.rowsWritten).toLocaleString()} 条记录）`);
          }
        } catch (e) { fail(e); }
      };
      void poll();
    } catch (e) { fail(e); }
  };
  const progress = current ? Math.min(100, Number(current.bytesRead) / Math.max(1, Number(current.totalBytes)) * 100) : 0;
  const runningExports = exports.filter(e => ["queued", "running"].includes(e.status));
  const contextSelection = contextMenu?.selection;
  const contextColumns = selectedColumns(contextSelection, visible.length);
  const contextColumn = contextColumns.length === 1 ? visible[contextColumns[0] - 1] : undefined;
  const menuActions: MenuAction[] = [
    { label: "复制所选内容", icon: <Copy size={15} />, run: () => void copySelection(contextSelection), disabled: !canRead, shortcut: "Ctrl+C" },
    { label: "编辑单元格", icon: <Pencil size={15} />, run: () => { if (contextMenu?.item) void activateCell(contextMenu.item); },
      disabled: !canEdit || !contextMenu?.item || contextMenu.item[0] === 0 || contextMenu.item[1] < 0, shortcut: "F2" },
    { label: "清空所选内容", icon: <Eraser size={15} />, run: () => void mutateSelection("clear", contextSelection), disabled: !canEdit, shortcut: "Delete" },
    { label: "筛选此列…", icon: <Filter size={15} />, run: () => { if (contextColumn) setFilterColumn(contextColumn.id); }, disabled: !contextColumn },
    { label: "隐藏所选列", icon: <EyeOff size={15} />, run: () => hideSelectedColumns(contextSelection), disabled: !contextColumns.length || contextColumns.length >= visible.length },
    { label: "显示全部隐藏列", icon: <Eye size={15} />, run: () => changeLayout({ hidden: [] }), disabled: !layout.hidden.length },
    { label: "删除所选行", icon: <Trash2 size={15} />, run: () => void mutateSelection("deleteRows", contextSelection), disabled: !canEdit || !!contextSelection?.columns.length, danger: true },
    { label: "删除所选列", icon: <Columns3 size={15} />, run: () => void mutateSelection("deleteColumns", contextSelection),
      disabled: !canEdit || !contextColumns.length || contextColumns.length >= liveColumns.length, danger: true },
    { label: "另存为 CSV…", icon: <Save size={15} />, run: () => setExportDialog(true), disabled: !canEdit, shortcut: "Ctrl+S" },
  ];

  return <div className="app-shell" onContextMenu={event => event.preventDefault()}>
    <header className="app-header">
      <div className="brand"><div className="brand-mark"><FileSpreadsheet size={22} strokeWidth={1.8} /></div><div><strong>CSVScope</strong><span>大型数据，轻松编辑</span></div><span className="version">PREVIEW {version}</span></div>
      <div className="header-actions"><span className="local-badge"><ShieldCheck size={14} />本地处理</span><button className="icon-button" aria-label="打开运行日志" title="打开运行日志" onClick={() => { if (native) void api.logs().then(path => setNotice(`运行日志：${path}`)).catch(fail); }}><FileText size={18} /></button><button className="icon-button" aria-label="缓存管理" title="缓存管理" onClick={() => { if (native) void api.usage().then(setCache).catch(fail); }}><HardDrive size={18} /></button><button className="icon-button" aria-label="使用帮助" onClick={() => setNotice("Ctrl+O 打开 · 表头倒三角筛选/排序 · Ctrl/Shift 多选行列 · 右键操作/标色 · 双击或 F2 编辑 · Ctrl+Z 撤销 · Ctrl+S 另存为 CSV")}><HelpCircle size={18} /></button></div>
    </header>
    <nav className="tab-bar">
      {tabs.map(tab => <div key={tab.id} className={`file-tab ${active === tab.id ? "active" : ""}`} onClick={() => setActive(tab.id)}>
        <FileText size={15} /><span title={tab.path}>{tab.name}{isUnsaved(tab) ? " *" : ""}</span>
        {["opening", "preparing", "previewing"].includes(tab.status) && <LoaderCircle size={13} className="spin" />}
        <button aria-label={`关闭 ${tab.name}`} onClick={e => { e.stopPropagation(); requestClose(tab); }}><X size={13} /></button>
      </div>)}
      <button className="new-tab" onClick={() => void openFiles()} aria-label="打开新文件">+</button>
      <span className="tab-spacer" /><span className="tab-caption">{tabs.length ? `${tabs.length} 个文件` : "本地 CSV 工作区"}</span>
    </nav>
    <section className="toolbar">
      <button className="button primary" onClick={() => void openFiles()}><FolderOpen size={16} />打开文件<span className="shortcut">Ctrl O</span></button>
      <button className="button subtle" onClick={() => setParseOpen(true)}><Settings2 size={15} />解析设置</button>
      <div className="toolbar-divider" />
      <button className="icon-button" title="撤销 Ctrl+Z" aria-label="撤销" disabled={!current?.canUndo} onClick={() => void history(false)}><Undo2 size={18} /></button>
      <button className="icon-button" title="重做 Ctrl+Y" aria-label="重做" disabled={!current?.canRedo} onClick={() => void history(true)}><Redo2 size={18} /></button>
      <div className="toolbar-divider" />
      <div className="relative"><button className="button subtle" disabled={!current} onClick={() => setColumnsOpen(!columnsOpen)}><Columns3 size={16} />列布局<ChevronDown size={12} /></button>
        {columnsOpen && current && <div className="columns-popover"><div className="popover-title">显示与固定列</div>
          <label className="freeze-field">固定前 <select value={layout.frozen} onChange={e => changeLayout({ frozen: Number(e.target.value) })}>{[0, 1, 2, 3, 4].map(n => <option key={n}>{n}</option>)}</select> 列</label>
          <div className="column-options">{liveColumns.map(c => <label key={c.id}><input type="checkbox" checked={!layout.hidden.includes(c.id)} disabled={visible.length === 1 && !layout.hidden.includes(c.id)} onChange={() => { clearSelection(); changeLayout({ hidden: layout.hidden.includes(c.id) ? layout.hidden.filter(id => id !== c.id) : [...layout.hidden, c.id] }); }} />{c.name}</label>)}</div>
          {!!layout.hidden.length && <button className="text-button" onClick={() => { clearSelection(); changeLayout({ hidden: [] }); }}>显示全部隐藏列</button>}
          <button className="text-button" onClick={() => setColumnsOpen(false)}>完成</button>
        </div>}
      </div>
      <span className="toolbar-spacer" />
      {current && <span className="edit-hint">{editBusy ? "正在应用修改…" : nativeReady ? "双击编辑 · 右键操作 · Ctrl/Shift 多选列" : "可浏览、查看和复制已加载记录 · 完整加载后可编辑"}</span>}
      <button className="button" disabled={!canEdit} onClick={() => setExportDialog(true)}><Save size={15} />另存为 CSV</button>
    </section>
    {current && <section className="query-bar">
      <div className="query-label"><Filter size={14} />筛选</div>
      {Object.keys(current.view.filters).length === 0 ? <span className="muted">{nativeReady ? "未设置条件 · 显示全部记录" : "未设置条件"}</span>
        : Object.entries(current.view.filters).map(([column, filter]) => <button className="filter-chip" key={column} onClick={() => setFilterColumn(Number(column))}><strong>{current.columns[Number(column)]?.name}</strong><span>{filterSummary(filter)}</span><X size={12} onClick={e => { e.stopPropagation(); const view = structuredClone(current.view); delete view.filters[column]; void commit(view).catch(fail); }} /></button>)}
      {!!current.view.sort.length && <span className="sort-chip">{current.view.sort[0].descending ? <ArrowDown size={12} /> : <ArrowUp size={12} />}{current.columns[current.view.sort[0].column]?.name}</span>}
      <span className="toolbar-spacer" />
      <span className="filter-legend"><ChevronDown size={12} />未筛选 <Filter size={12} />已筛选</span>
      {!!Object.keys(current.view.filters).length && <button className="text-button" onClick={() => void commit({ ...current.view, filters: {} }).catch(fail)}>清除全部</button>}
    </section>}
    {error && <div className="error-banner"><span>{error}</span><button className="icon-button" onClick={() => setError("")}><X size={15} /></button></div>}
    {!!current?.warnings.length && <div className="preparation-banner"><span>{current.warnings.join(" ")}</span></div>}
    {current && current.status !== "ready" && <div className={`preparation-banner ${["failed", "stale"].includes(current.status) ? "failed" : ""}`}>
      {["preparing", "opening", "previewing", "cancelling"].includes(current.status) && <LoaderCircle size={16} className="spin" />}
      <span><strong>{current.status === "preparing" ? loadingRows ? `后台加载中 · 已加载 ${Number(current.rowsRead).toLocaleString()} 条${hasFilters ? ` · 已匹配 ${generated.toLocaleString()} 条` : ""}` : "首屏预览 / 后台加载中" : current.phase}</strong><span>{current.error ?? `${formatBytes(current.bytesRead)} / ${formatBytes(current.totalBytes)}；筛选已作用于加载的数据，新命中会继续补充。${current.view.sort.length ? "当前按源记录顺序显示，完整加载后执行全局排序。" : "可浏览、查看和复制结果。"}`}</span></span>
      {["preparing", "opening", "previewing"].includes(current.status) && <button className="button" onClick={() => void api.cancel(current.id).then(refresh).catch(fail)}>取消</button>}
      {["failed", "cancelled", "stale"].includes(current.status) && <button className="button" onClick={() => { void api.open(current.path, current.parseSpec).then(id => { setActive(id); return refresh(); }).catch(fail); }}>重新打开</button>}
      <div className="preparation-progress" style={{ width: `${progress}%` }} />
    </div>}
    <main className="workspace">
      {!current ? <div className="empty-state">
        <div className="empty-illustration"><div className="sheet-grid">{Array.from({ length: 20 }, (_, i) => <span key={i} />)}</div><div className="sheet-label">CSV</div><div className="sheet-spark"><ZoomIn size={25} /></div></div>
        <h1>让大文件，回到轻松浏览</h1><p>编辑、联动筛选、标记和保存大型 CSV。<br />数据始终留在你的电脑上。</p>
        <button className="button primary large" onClick={() => void openFiles()}><FolderOpen size={18} />打开 CSV 文件</button>
        <span className="empty-shortcut">或按 <kbd>Ctrl</kbd> + <kbd>O</kbd></span>
        <div className="feature-notes"><span><FileText size={15} />流式读取</span><span><Filter size={15} />多列联动筛选</span><span><Database size={15} />磁盘查询缓存</span></div>
        {!native && <div className="browser-note">界面预览 · 本地文件功能请在桌面版使用</div>}
      </div> : <div className="grid-shell" data-revision={current.revision} data-query-status={query?.status}
        onMouseDownCapture={() => { selectionBeforePointer.current = selection; }}>
        <DataEditor ref={grid} columns={columns} rows={rows} getCellContent={getCell} width="100%" height="100%"
          rowHeight={30} headerHeight={37} rowMarkers="none"
          freezeColumns={Math.min(layout.frozen, visible.length) + 1} smoothScrollX smoothScrollY
          onVisibleRegionChanged={visibleRegionChanged} gridSelection={selection ?? emptyGridSelection} onGridSelectionChange={selectGrid}
          columnSelect="multi" rowSelect="multi" rangeSelect="multi-rect"
          onCellContextMenu={(item, event) => { event.preventDefault(); showCellMenu(item, event.bounds.x + event.localEventX, event.bounds.y + event.localEventY); }}
          onHeaderContextMenu={(index, event) => {
            event.preventDefault();
            const target = selection?.columns.hasIndex(index) ? selection
              : index === 0 ? { ...emptyGridSelection, rows: CompactSelection.fromSingleSelection([0, rows]) }
              : { ...emptyGridSelection, columns: CompactSelection.fromSingleSelection(index) };
            selectGrid(target);
            setContextMenu({ x: event.bounds.x + event.localEventX, y: event.bounds.y + event.localEventY, selection: target, item: [index, -1] });
          }}
          onCellClicked={([column, row], event) => {
            if (column !== 0) return;
            event.preventDefault();
            // Glide changes selection on mouse-down, before this click callback.
            const previous = selectionBeforePointer.current?.rows ?? CompactSelection.empty();
            const nextRows = event.shiftKey ? CompactSelection.fromSingleSelection([Math.min(previous.first() ?? row, row), Math.max(previous.first() ?? row, row) + 1])
              : event.ctrlKey || event.metaKey ? previous.hasIndex(row) ? previous.remove(row) : previous.add(row) : CompactSelection.fromSingleSelection(row);
            selectGrid({ ...emptyGridSelection, rows: nextRows });
          }}
          onDelete={target => { void mutateSelection("clear", target); return false; }}
          onPaste={(target, values) => { void pasteCells(target, values); return false; }}
          onColumnResize={(column, width) => { if (column.id !== "source-rid") changeLayout({ widths: { ...layout.widths, [Number(column.id)]: width } }); }}
          onHeaderMenuClick={index => { if (index > 0) setFilterColumn(visible[index - 1].id); }}
          onHeaderClicked={(index, event) => {
            event.preventDefault();
            if (index === 0) { selectGrid({ ...emptyGridSelection, rows: CompactSelection.fromSingleSelection([0, rows]) }); return; }
            if (event.localEventX >= event.bounds.width - 30) { setFilterColumn(visible[index - 1].id); return; }
            const previous = selectionBeforePointer.current?.columns ?? CompactSelection.empty();
            const nextColumns = event.shiftKey ? CompactSelection.fromSingleSelection([Math.min(previous.first() ?? index, index), Math.max(previous.first() ?? index, index) + 1])
              : event.ctrlKey || event.metaKey ? previous.hasIndex(index) ? previous.remove(index) : previous.add(index) : CompactSelection.fromSingleSelection(index);
            selectGrid({ ...emptyGridSelection, columns: nextColumns });
          }}
          drawHeader={({ ctx, columnIndex, rect, isSelected, menuBounds }, drawContent) => {
            if (columnIndex === 0) { drawContent(); return; }
            const column = visible[columnIndex - 1];
            if (!column) return;
            const filtered = !!current.view.filters[column.id];
            const sort = current.view.sort.find(key => key.column === column.id);
            ctx.save();
            ctx.beginPath(); ctx.rect(rect.x, rect.y, rect.width, rect.height); ctx.clip();
            ctx.fillStyle = filtered ? "#e2f1e9" : isSelected ? "#e9f3ef" : "#f6f8fa";
            ctx.fillRect(rect.x, rect.y, rect.width, rect.height);
            ctx.fillStyle = filtered ? "#176b5b" : "#34454e";
            ctx.font = "600 12px 'Segoe UI', 'Microsoft YaHei', sans-serif";
            ctx.textBaseline = "middle";
            const suffix = sort ? sort.descending ? " ↓" : " ↑" : "";
            let title = column.name;
            const availableWidth = Math.max(0, rect.width - 52 - ctx.measureText(suffix).width);
            while (title.length && ctx.measureText(title).width > availableWidth) title = title.slice(0, -1);
            if (title !== column.name && title.length > 1) title = title.slice(0, -1) + "…";
            ctx.fillText(title + suffix, rect.x + 12, rect.y + rect.height / 2);
            const cx = menuBounds.x + menuBounds.width / 2;
            const cy = rect.y + rect.height / 2;
            ctx.fillStyle = filtered ? "#176b5b" : "#ffffff";
            ctx.strokeStyle = filtered ? "#176b5b" : "#c5d0d5";
            ctx.lineWidth = 1;
            ctx.beginPath(); ctx.roundRect(cx - 10, cy - 10, 20, 20, 3); ctx.fill(); ctx.stroke();
            ctx.fillStyle = filtered ? "#ffffff" : "#647780";
            ctx.beginPath();
            if (filtered) {
              ctx.moveTo(cx - 6, cy - 4); ctx.lineTo(cx + 6, cy - 4); ctx.lineTo(cx + 2, cy + 1);
              ctx.lineTo(cx + 2, cy + 5); ctx.lineTo(cx - 1, cy + 7); ctx.lineTo(cx - 1, cy + 1);
            } else {
              ctx.moveTo(cx - 4, cy - 2); ctx.lineTo(cx + 4, cy - 2); ctx.lineTo(cx, cy + 3);
            }
            ctx.closePath(); ctx.fill();
            ctx.strokeStyle = "#e1e7eb"; ctx.beginPath(); ctx.moveTo(rect.x + rect.width - .5, rect.y);
            ctx.lineTo(rect.x + rect.width - .5, rect.y + rect.height); ctx.stroke();
            ctx.restore();
          }}
          onCellActivated={item => void activateCell(item)}
          getCellsForSelection={range => async () => {
            if (!current || !query) return [];
            try {
              const selectedColumns = visible.slice(Math.max(0, range.x - 1), range.x + range.width - 1).map(c => c.id);
              const data = await api.copy(current.id, query.id, range.y + windowStart, range.height, selectedColumns, range.x === 0);
              return data.map(row => row.map(value => ({ kind: GridCellKind.Text as const, data: value, displayData: value, readonly: true, allowOverlay: false })));
            } catch (e) { fail(e); return []; }
          }}
          theme={{ accentColor: "#176b5b", accentLight: "#e8f4ef", textDark: "#26323c", textMedium: "#64737e", textLight: "#97a3ab",
            bgCell: "#ffffff", bgHeader: "#f6f8fa", bgHeaderHovered: "#edf1f4", bgHeaderHasFocus: "#e9f3ef",
            borderColor: "#e7ebef", horizontalBorderColor: "#eef1f4", fontFamily: "'Segoe UI', 'Microsoft YaHei', sans-serif",
            baseFontStyle: "12px", headerFontStyle: "600 12px", cellHorizontalPadding: 12 }}
        />
        {(nativeReady || loadingRows) && generated === 0 && <div className="grid-overlay">
          {query?.status === "ready" ? <><Filter size={30} /><strong>没有匹配记录</strong><span>放宽条件或清除筛选后重试</span></>
            : ["failed", "cancelled"].includes(query?.status ?? "") ? <><Filter size={30} /><strong>{query?.status === "cancelled" ? "查询已取消" : "查询失败"}</strong><span>条件已保留，可重试或修改条件</span></>
            : loadingRows ? <><LoaderCircle className="spin" size={26} /><strong>{Number(query?.scannedRows ?? 0) < Number(current.rowsRead) ? "正在筛选已加载记录" : "已加载部分暂未找到匹配记录"}</strong><span>后台继续读取，新命中会自动显示；当前数量不是全文件结果</span></>
            : <><LoaderCircle className="spin" size={26} /><strong>{query?.status === "queued" ? "等待查询资源" : "正在生成完整数据结果"}</strong><span>排序或完整扫描可能需要一些时间</span></>}
        </div>}
        {query?.error && <div className="query-error">{query.error}<button className="button" onClick={() => { void api.projection(current.id, current.revision, query.projection).then(refresh).catch(fail); }}>重试</button></div>}
      </div>}
    </main>
    {runningExports.length > 0 && <div className="export-progress">{runningExports.map(e => <div key={e.id}><LoaderCircle className="spin" size={15} /><span>导出 {Number(e.rowsWritten).toLocaleString()} / {Number(e.totalRows).toLocaleString()} 条</span><button className="text-button" onClick={() => void api.cancelExport(e.id)}>取消</button></div>)}</div>}
    <footer className="status-bar">
      <div className="status-left"><span className={`status-dot ${busy ? "busy" : ""}`} />{current ? current.status === "ready" ? query?.exactTotal !== null && query?.exactTotal !== undefined ? `${Number(query.exactTotal).toLocaleString()} 条结果 / ${Number(current.rowsRead).toLocaleString()} 条记录`
        : ["failed", "cancelled"].includes(query?.status ?? "") ? `查询${query?.status === "cancelled" ? "已取消" : "失败"} · 已生成 ${generated.toLocaleString()} 条（非完整结果）`
        : `已生成 ${generated.toLocaleString()} 条 · 完整数量计算中` : loadingRows ? `${hasFilters ? `已匹配 ${generated.toLocaleString()} 条 / ` : ""}已加载 ${Number(current.rowsRead).toLocaleString()} 条 · 后台继续加载 · 总数待完成${current.view.sort.length ? " · 全局排序待加载完成" : ""}` : `${current.preview.length} 条预览 · ${current.phase}` : "就绪"}
        {current && <><span className="status-separator" /><span>{current.parseSpec.encoding.toUpperCase()}</span><span>{current.parseSpec.delimiter === "\t" ? "Tab" : current.parseSpec.delimiter === ";" ? "分号" : current.parseSpec.delimiter === "|" ? "竖线" : "逗号"}分隔</span><span>{visible.length} / {liveColumns.length} 列可见</span>{current.modified && <span className="modified-status">{isUnsaved(current) ? "已修改 · 请另存为" : "已另存为副本"}</span>}{current.cacheHit && <span className="cache-hit"><Check size={11} />已验证缓存</span>}</>}
      </div>
      {current && <div className="jump-control"><span>{loadingRows ? hasFilters ? "跳转至已匹配行" : "跳转至已加载行" : "跳转至结果行"}</span><input aria-label="跳转行号" placeholder="行号" value={jump} onChange={e => setJump(e.target.value)} onKeyDown={e => { if (e.key === "Enter") gotoRow(); }} /><button onClick={gotoRow}>前往</button></div>}
      {current && ["queued", "building"].includes(query?.status ?? "") && <button className="text-button" onClick={() => void api.cancel(current.id).then(refresh).catch(fail)}>取消查询</button>}
      {!current && <span className="status-local"><ShieldCheck size={12} />离线工作 · 原文保真</span>}
    </footer>
    {notice && <div className="toast">{notice}</div>}
    {contextMenu && <ContextMenu x={contextMenu.x} y={contextMenu.y}
      title={contextSelection?.columns.length ? `已选择 ${contextColumns.length} 列` : contextSelection?.rows.length ? `已选择 ${contextSelection.rows.length} 行` : "所选单元格"}
      actions={menuActions} canColor={!!canEdit} onColor={color => void mutateSelection("color", contextSelection, color)} onClose={closeContextMenu} />}
    {filterColumn !== null && current && <FilterMenu key={`${current.id}:${filterColumn}`} tab={current} column={filterColumn} onClose={() => setFilterColumn(null)} onApply={async (filter, sort) => {
      const view = structuredClone(current.view);
      if (filter) view.filters[filterColumn] = filter; else delete view.filters[filterColumn];
      if (sort) view.sort = sort;
      await commit(view);
    }} />}
    {cell && <div className="modal-scrim"><section className="dialog cell-dialog"><header><h2>{cell.name}</h2><button className="icon-button" onClick={() => setCell(null)}><X size={18} /></button></header>
      {cell.loading ? <LoaderCircle className="spin" /> : <textarea aria-label="单元格内容" autoFocus readOnly={!canEdit} value={cell.value} spellCheck={false}
        onChange={event => setCell(old => old ? { ...old, value: event.target.value } : old)}
        onKeyDown={event => {
          if (event.key === "Escape") setCell(null);
          if (event.ctrlKey && event.key === "Enter") { event.preventDefault(); void saveCell(); }
        }} />}
      <footer><span>{cell.value.length.toLocaleString()} 个字符 · {canEdit ? "Ctrl+Enter 应用" : "完整加载后可编辑"}</span>
        <button className="button" disabled={cell.loading} onClick={() => { void navigator.clipboard.writeText(cell.value).then(() => setNotice("已复制完整内容")).catch(fail); }}>复制完整内容</button>
        <button className="button primary" disabled={!canEdit || cell.loading || cell.value === cell.original} onClick={() => void saveCell()}><Check size={14} />应用修改</button></footer>
    </section></div>}
    {parseOpen && <div className="modal-scrim"><section className="dialog settings-dialog"><header><h2>解析设置</h2><button className="icon-button" onClick={() => setParseOpen(false)}><X size={18} /></button></header><p className="muted">以下设置用于下一次打开文件。更改设置后重新打开会创建新的数据版本。</p>
      <label>文件编码<select value={parseSpec.encoding} onChange={e => setParseSpec(s => ({ ...s, encoding: e.target.value }))}><option value="auto">自动识别</option><option value="utf-8">UTF-8 / UTF-8 BOM</option><option value="gbk">GBK</option><option value="gb18030">GB18030（WHATWG 映射）</option></select></label>
      <label>分隔符<select value={parseSpec.delimiter ?? "auto"} onChange={e => setParseSpec(s => ({ ...s, delimiter: e.target.value === "auto" ? null : e.target.value }))}><option value="auto">自动识别</option><option value=",">逗号 ,</option><option value=";">分号 ;</option><option value={"\t"}>制表符 Tab</option><option value="|">竖线 |</option></select></label>
      <label className="check-line"><input type="checkbox" checked={parseSpec.hasHeaders} onChange={e => setParseSpec(s => ({ ...s, hasHeaders: e.target.checked }))} />第一条记录为表头</label>
      <div className="policy-note">保留空格与原文字段；空字段为空文本。列数不一致、非法编码或超出记录预算时停止并定位，不跳过。</div>
      <footer><span>记录上限 16MB · 字段上限 8MB</span><button className="button primary" onClick={() => setParseOpen(false)}>保存设置</button></footer>
    </section></div>}
    {exportDialog && current && <div className="modal-scrim"><section className="dialog settings-dialog" role="dialog" aria-label="另存为 CSV"><header><h2>另存为 CSV</h2><button className="icon-button" onClick={() => setExportDialog(false)}><X size={18} /></button></header>
      <p className="muted">保存修改后的 {Number(exportRows === "all" ? Number(current.rowsRead) - editState(current.view).deletedRows.length : query?.exactTotal ?? 0).toLocaleString()} 条记录，遵循当前排序。已删除的行列不会写入。</p>
      <label>行范围<select aria-label="保存行范围" value={exportRows} onChange={e => setExportRows(e.target.value)}><option value="all">全部记录（忽略筛选）</option><option value="filtered">当前筛选结果</option></select></label>
      <label>列范围<select aria-label="保存列范围" value={exportScope} onChange={e => setExportScope(e.target.value)}><option value="all">全部保留列（包含隐藏列）</option><option value="visible">当前可见列（显示顺序）</option></select></label>
      <label>编码<select value={exportEncoding} onChange={e => setExportEncoding(e.target.value)}><option value="utf-8">UTF-8</option><option value="gbk">GBK</option><option value="gb18030">GB18030</option></select></label>
      <label className="check-line"><input type="checkbox" checked={exportBom} onChange={e => setExportBom(e.target.checked)} disabled={exportEncoding !== "utf-8"} />写入 UTF-8 BOM</label>
      <div className="policy-note">保存为新的 .csv 文件，请使用新文件名。有颜色标记时，同时生成 .csv.csvscope.json 文件；重新打开 CSV 会自动恢复颜色，移动文件时请保留这两个文件。</div>
      <footer><button className="button" onClick={() => setExportDialog(false)}>取消</button><button className="button primary" onClick={() => void startExport()}><Download size={15} />选择保存位置</button></footer>
    </section></div>}
    {closeTab && <div className="modal-scrim"><section className="dialog" role="dialog" aria-label="关闭已修改文件"><header><h2>关闭已修改文件？</h2></header>
      <p>{closeTab.name} 包含尚未完整保存的内容或颜色标记。</p>
      <footer><button className="button" onClick={() => setCloseTab(null)}>取消</button>
        <button className="button danger-text" onClick={() => { void api.close(closeTab.id).then(refresh).catch(fail); setCloseTab(null); }}>放弃更改并关闭</button>
        <button className="button primary" onClick={() => { setActive(closeTab.id); setCloseTab(null); setExportDialog(true); }}>先另存为 CSV</button></footer>
    </section></div>}
    {closeApp && <div className="modal-scrim"><section className="dialog" role="dialog" aria-label="退出前保存"><header><h2>还有文件尚未保存</h2></header>
      <p>{unsavedTabs.current.map(tab => tab.name).join("、")} 包含修改。可以返回工作区另存为 CSV。</p>
      <footer><button className="button primary" onClick={() => setCloseApp(false)}>返回保存</button>
        <button className="button danger-text" onClick={() => { void getCurrentWindow().destroy().catch(fail); }}>放弃更改并退出</button></footer>
    </section></div>}
    {cache && <div className="modal-scrim"><section className="dialog settings-dialog"><header><h2>磁盘缓存</h2><button className="icon-button" onClick={() => setCache(null)}><X size={18} /></button></header>
      <p className="cache-path">{cache.path}</p><div className="cache-stats"><div><span>可复用文件缓存</span><strong>{formatBytes(cache.datasets)}</strong></div>
        <div><span>筛选 / 排序 / 候选缓存</span><strong>{formatBytes(cache.results)}</strong></div>
        <div><span>DuckDB 临时文件</span><strong>{formatBytes(cache.temporary)}</strong></div>
        <div><span>准备中的文件缓存</span><strong>{formatBytes(cache.imports)}</strong></div>
        <div><span>总配额</span><strong>{formatBytes(cache.limit)}</strong></div></div>
      <p className="small-note">清理只删除未被打开标签使用的完整文件缓存。</p><footer><button className="button" onClick={() => { void api.clearCache().then(async n => { setNotice(`已清理 ${formatBytes(n)}`); setCache(await api.usage()); }).catch(fail); }}>清理闲置缓存</button><button className="button primary" onClick={() => setCache(null)}>完成</button></footer>
    </section></div>}
  </div>;
}
