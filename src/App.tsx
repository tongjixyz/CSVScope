import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import DataEditor, { CompactSelection, GridCellKind, type DataEditorRef, type GridCell, type GridColumn, type GridSelection, type Item, type Rectangle } from "@glideapps/glide-data-grid";
import type { Table } from "apache-arrow";
import {
  ArrowDown, ArrowUp, Check, ChevronDown, Columns3, Database, Download, FileSpreadsheet,
  FileText, Filter, FolderOpen, HardDrive, HelpCircle, LoaderCircle, Redo2, Settings2, ShieldCheck,
  Undo2, X, ZoomIn,
} from "lucide-react";
import { api, chooseExport, chooseFiles, native } from "./api";
import { reportFrontendError } from "./diagnostics";
import FilterMenu from "./FilterMenu";
import { filterSummary, formatBytes } from "./selection";
import { defaultParseSpec, type CacheUsage, type ExportStatus, type Layout, type ParseSpec, type Session, type ViewState } from "./types";

const defaultLayout = (): Layout => ({ hidden: [], widths: {}, frozen: 0, start: 0, scrollRow: 0, scrollColumn: 0 });
const PAGE_SIZE = 256;
const WINDOW_SIZE = 200_000;
const emptyGridSelection: GridSelection = { columns: CompactSelection.empty(), rows: CompactSelection.empty() };

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
  const [cell, setCell] = useState<{ name: string; value: string; loading: boolean } | null>(null);
  const [exportDialog, setExportDialog] = useState(false);
  const [exportScope, setExportScope] = useState("all");
  const [exportEncoding, setExportEncoding] = useState("utf-8");
  const [exportBom, setExportBom] = useState(false);
  const [exports, setExports] = useState<ExportStatus[]>([]);
  const [cache, setCache] = useState<CacheUsage | null>(null);
  const [selection, setSelection] = useState<GridSelection>();
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
  const layout = active ? layouts[active] ?? defaultLayout() : defaultLayout();
  const visible = useMemo(() => current?.columns.filter(c => !layout.hidden.includes(c.id)) ?? [], [current?.columns, layout.hidden]);
  const nativeReady = current?.status === "ready";
  const query = current?.query?.revision === current?.revision ? current?.query : undefined;
  queryRef.current = query?.id;
  const generated = Number(query?.generatedRows ?? 0);
  const previewMode = !nativeReady;
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
  const busy = tabs.some(t => ["opening", "previewing", "preparing", "cancelling"].includes(t.status)
    || (t.status === "ready" && (!t.query || t.query.revision !== t.revision)) || ["queued", "building"].includes(t.query?.status ?? ""));
  useEffect(() => {
    if (!native) return;
    const timer = setInterval(() => void refresh(), busy ? 350 : 1200);
    return () => clearInterval(timer);
  }, [busy, refresh]);
  useLayoutEffect(() => {
    activeRef.current = active;
    pages.current.clear(); setSelection(active ? selections.current.get(active) : undefined); setFilterColumn(null);
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
    if (!current || !query || !nativeReady || !projection.length) return;
    if (projection.some(c => !query.projection.includes(c))) {
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
  }, [current?.id, query?.id, query?.status, query?.generatedRows, query?.projection, nativeReady, projection, region.y, region.height, windowStart, generated, refresh, fail, visible]);

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
    return { kind: GridCellKind.Text, data: text, displayData: text, readonly: true, allowOverlay: false };
  }, [current, visible, previewMode, windowStart, query?.id, projection]);

  const columns: GridColumn[] = [{ id: "source-rid", title: "源记录", width: 84, hasMenu: false }, ...visible.map(c => ({
    id: String(c.id), title: c.name,
    width: layout.widths[c.id] ?? (c.name.length > 16 ? 220 : 170),
    hasMenu: true,
    icon: current?.view.filters[c.id] ? "headerBoolean" : undefined,
  }))];
  const activateCell = async ([columnIndex, row]: Item) => {
    const column = visible[columnIndex - 1];
    if (!current || !column) return;
    setCell({ name: `${column.name} · 记录 ${row + windowStart + 1}`, value: "", loading: true });
    try {
      let value: string;
      if (previewMode) value = current.preview[row + windowStart]?.[column.id] ?? "";
      else {
        const absolute = row + windowStart;
        const table = pages.current.get(`${query?.id}:${Math.floor(absolute / PAGE_SIZE)}:${projection.join(",")}`);
        const rid = table?.getChild("_source_rid")?.get(absolute % PAGE_SIZE);
        if (rid === undefined || rid === null) throw new Error("单元格仍在加载");
        value = await api.cell(current.id, String(rid), column.id);
      }
      setCell({ name: `${column.name} · ${previewMode ? "预览内容" : "完整内容"}`, value, loading: false });
    } catch (e) { setCell(null); fail(e); }
  };
  const changeLayout = (next: Partial<Layout>) => {
    if (!active) return;
    setLayouts(old => ({ ...old, [active]: { ...(old[active] ?? defaultLayout()), ...next } }));
  };
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
      setNotice(query?.status === "ready" ? "行号超出结果范围" : "该位置尚未生成，请等待完整结果");
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
      const exportId = await api.export(current, path, exportScope === "all" ? current.columns.map(c => c.id) : visible.map(c => c.id), exportBom, exportEncoding);
      setExportDialog(false);
      const poll = async () => {
        try {
          const status = await api.exportStatus(exportId);
          if (!mounted.current) return;
          setExports(old => [...old.filter(e => e.id !== status.id), status]);
          if (["queued", "running"].includes(status.status)) setTimeout(() => void poll(), 400);
          else if (status.error) fail(status.error);
          else if (status.status === "completed") setNotice(`导出完成：${Number(status.rowsWritten).toLocaleString()} 条记录`);
        } catch (e) { fail(e); }
      };
      void poll();
    } catch (e) { fail(e); }
  };
  const progress = current ? Math.min(100, Number(current.bytesRead) / Math.max(1, Number(current.totalBytes)) * 100) : 0;
  const runningExports = exports.filter(e => ["queued", "running"].includes(e.status));

  return <div className="app-shell">
    <header className="app-header">
      <div className="brand"><div className="brand-mark"><FileSpreadsheet size={22} strokeWidth={1.8} /></div><div><strong>CSVScope</strong><span>大型数据，轻松查看</span></div><span className="version">PREVIEW 0.1.1</span></div>
      <div className="header-actions"><span className="local-badge"><ShieldCheck size={14} />本地处理</span><button className="icon-button" aria-label="打开运行日志" title="打开运行日志" onClick={() => { if (native) void api.logs().then(path => setNotice(`运行日志：${path}`)).catch(fail); }}><FileText size={18} /></button><button className="icon-button" aria-label="缓存管理" title="缓存管理" onClick={() => { if (native) void api.usage().then(setCache).catch(fail); }}><HardDrive size={18} /></button><button className="icon-button" aria-label="使用帮助" onClick={() => setNotice("Ctrl+O 打开文件 · 点击表头排序 · 表头菜单筛选 · Ctrl+Z 撤销 · 双击单元格查看完整内容")}><HelpCircle size={18} /></button></div>
    </header>
    <nav className="tab-bar">
      {tabs.map(tab => <div key={tab.id} className={`file-tab ${active === tab.id ? "active" : ""}`} onClick={() => setActive(tab.id)}>
        <FileText size={15} /><span title={tab.path}>{tab.name}</span>
        {["opening", "preparing", "previewing"].includes(tab.status) && <LoaderCircle size={13} className="spin" />}
        <button aria-label={`关闭 ${tab.name}`} onClick={e => { e.stopPropagation(); void api.close(tab.id).then(refresh).catch(fail); }}><X size={13} /></button>
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
          <div className="column-options">{current.columns.map(c => <label key={c.id}><input type="checkbox" checked={!layout.hidden.includes(c.id)} disabled={visible.length === 1 && !layout.hidden.includes(c.id)} onChange={() => changeLayout({ hidden: layout.hidden.includes(c.id) ? layout.hidden.filter(id => id !== c.id) : [...layout.hidden, c.id] })} />{c.name}</label>)}</div>
          <button className="text-button" onClick={() => setColumnsOpen(false)}>完成</button>
        </div>}
      </div>
      <span className="toolbar-spacer" />
      <button className="button" disabled={!current || query?.status !== "ready" || current.status !== "ready"} onClick={() => setExportDialog(true)}><Download size={15} />导出结果</button>
    </section>
    {current && <section className="query-bar">
      <div className="query-label"><Filter size={14} />筛选</div>
      {Object.keys(current.view.filters).length === 0 ? <span className="muted">{nativeReady ? "未设置条件 · 显示全部记录" : "未设置条件"}</span>
        : Object.entries(current.view.filters).map(([column, filter]) => <button className="filter-chip" key={column} onClick={() => setFilterColumn(Number(column))}><strong>{current.columns[Number(column)]?.name}</strong><span>{filterSummary(filter)}</span><X size={12} onClick={e => { e.stopPropagation(); const view = structuredClone(current.view); delete view.filters[column]; void commit(view).catch(fail); }} /></button>)}
      {!!current.view.sort.length && <span className="sort-chip">{current.view.sort[0].descending ? <ArrowDown size={12} /> : <ArrowUp size={12} />}{current.columns[current.view.sort[0].column]?.name}</span>}
      <span className="toolbar-spacer" />
      {!!Object.keys(current.view.filters).length && <button className="text-button" onClick={() => void commit({ ...current.view, filters: {} }).catch(fail)}>清除全部</button>}
    </section>}
    {error && <div className="error-banner"><span>{error}</span><button className="icon-button" onClick={() => setError("")}><X size={15} /></button></div>}
    {!!current?.warnings.length && <div className="preparation-banner"><span>{current.warnings.join(" ")}</span></div>}
    {current && current.status !== "ready" && <div className={`preparation-banner ${["failed", "stale"].includes(current.status) ? "failed" : ""}`}>
      {["preparing", "opening", "previewing", "cancelling"].includes(current.status) && <LoaderCircle size={16} className="spin" />}
      <span><strong>{current.status === "preparing" ? "预览 / 准备中" : current.phase}</strong><span>{current.error ?? `${current.phase} · ${formatBytes(current.bytesRead)} / ${formatBytes(current.totalBytes)}；筛选将在完整数据就绪后执行。`}</span></span>
      {["preparing", "opening", "previewing"].includes(current.status) && <button className="button" onClick={() => void api.cancel(current.id).then(refresh).catch(fail)}>取消</button>}
      {["failed", "cancelled", "stale"].includes(current.status) && <button className="button" onClick={() => { void api.open(current.path, current.parseSpec).then(id => { setActive(id); return refresh(); }).catch(fail); }}>重新打开</button>}
      <div className="preparation-progress" style={{ width: `${progress}%` }} />
    </div>}
    <main className="workspace">
      {!current ? <div className="empty-state">
        <div className="empty-illustration"><div className="sheet-grid">{Array.from({ length: 20 }, (_, i) => <span key={i} />)}</div><div className="sheet-label">CSV</div><div className="sheet-spark"><ZoomIn size={25} /></div></div>
        <h1>让大文件，回到轻松浏览</h1><p>查看、联动筛选和导出大型 CSV。<br />数据始终留在你的电脑上。</p>
        <button className="button primary large" onClick={() => void openFiles()}><FolderOpen size={18} />打开 CSV 文件</button>
        <span className="empty-shortcut">或按 <kbd>Ctrl</kbd> + <kbd>O</kbd></span>
        <div className="feature-notes"><span><FileText size={15} />流式读取</span><span><Filter size={15} />多列联动筛选</span><span><Database size={15} />磁盘查询缓存</span></div>
        {!native && <div className="browser-note">界面预览 · 本地文件功能请在桌面版使用</div>}
      </div> : <div className="grid-shell">
        <DataEditor ref={grid} columns={columns} rows={rows} getCellContent={getCell} width="100%" height="100%"
          rowHeight={30} headerHeight={37} rowMarkers="none"
          freezeColumns={Math.min(layout.frozen, visible.length) + 1} smoothScrollX smoothScrollY
          onVisibleRegionChanged={visibleRegionChanged} gridSelection={selection ?? emptyGridSelection} onGridSelectionChange={next => { setSelection(next); if (active) selections.current.set(active, next); }}
          onColumnResize={(column, width) => { if (column.id !== "source-rid") changeLayout({ widths: { ...layout.widths, [Number(column.id)]: width } }); }}
          onHeaderMenuClick={index => { if (index > 0) setFilterColumn(visible[index - 1].id); }}
          onHeaderClicked={index => {
            const column = visible[index - 1]?.id;
            if (column === undefined) return;
            const previous = current.view.sort.find(s => s.column === column);
            const sort = previous?.descending ? [] : [{ column, descending: previous ? true : false, interpretation: "text" as const }];
            void commit({ ...current.view, sort }).catch(fail);
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
        {nativeReady && generated === 0 && <div className="grid-overlay">
          {query?.status === "ready" ? <><Filter size={30} /><strong>没有匹配记录</strong><span>放宽条件或清除筛选后重试</span></>
            : ["failed", "cancelled"].includes(query?.status ?? "") ? <><Filter size={30} /><strong>{query?.status === "cancelled" ? "查询已取消" : "查询失败"}</strong><span>条件已保留，可重试或修改条件</span></>
            : <><LoaderCircle className="spin" size={26} /><strong>{query?.status === "queued" ? "等待查询资源" : "正在生成完整数据结果"}</strong><span>排序或完整扫描可能需要一些时间</span></>}
        </div>}
        {query?.error && <div className="query-error">{query.error}<button className="button" onClick={() => { void api.projection(current.id, current.revision, query.projection).then(refresh).catch(fail); }}>重试</button></div>}
      </div>}
    </main>
    {runningExports.length > 0 && <div className="export-progress">{runningExports.map(e => <div key={e.id}><LoaderCircle className="spin" size={15} /><span>导出 {Number(e.rowsWritten).toLocaleString()} / {Number(e.totalRows).toLocaleString()} 条</span><button className="text-button" onClick={() => void api.cancelExport(e.id)}>取消</button></div>)}</div>}
    <footer className="status-bar">
      <div className="status-left"><span className={`status-dot ${busy ? "busy" : ""}`} />{current ? current.status === "ready" ? query?.exactTotal !== null && query?.exactTotal !== undefined ? `${Number(query.exactTotal).toLocaleString()} 条结果 / ${Number(current.rowsRead).toLocaleString()} 条记录`
        : ["failed", "cancelled"].includes(query?.status ?? "") ? `查询${query?.status === "cancelled" ? "已取消" : "失败"} · 已生成 ${generated.toLocaleString()} 条（非完整结果）`
        : `已生成 ${generated.toLocaleString()} 条 · 完整数量计算中` : `${current.preview.length} 条预览 · ${current.phase}` : "就绪"}
        {current && <><span className="status-separator" /><span>{current.parseSpec.encoding.toUpperCase()}</span><span>{current.parseSpec.delimiter === "\t" ? "Tab" : current.parseSpec.delimiter === ";" ? "分号" : "逗号"}分隔</span><span>{current.columns.length} 列</span>{current.cacheHit && <span className="cache-hit"><Check size={11} />已验证缓存</span>}</>}
      </div>
      {current && <div className="jump-control"><span>跳转至结果行</span><input aria-label="跳转行号" placeholder="行号" value={jump} onChange={e => setJump(e.target.value)} onKeyDown={e => { if (e.key === "Enter") gotoRow(); }} /><button onClick={gotoRow}>前往</button></div>}
      {current && ["queued", "building"].includes(query?.status ?? "") && <button className="text-button" onClick={() => void api.cancel(current.id).then(refresh).catch(fail)}>取消查询</button>}
      {!current && <span className="status-local"><ShieldCheck size={12} />离线工作 · 原文保真</span>}
    </footer>
    {notice && <div className="toast">{notice}</div>}
    {filterColumn !== null && current && <FilterMenu key={`${current.id}:${filterColumn}`} tab={current} column={filterColumn} onClose={() => setFilterColumn(null)} onApply={async (filter, sort) => {
      const view = structuredClone(current.view);
      if (filter) view.filters[filterColumn] = filter; else delete view.filters[filterColumn];
      if (sort) view.sort = sort;
      await commit(view);
    }} />}
    {cell && <div className="modal-scrim"><section className="dialog cell-dialog"><header><h2>{cell.name}</h2><button className="icon-button" onClick={() => setCell(null)}><X size={18} /></button></header>
      {cell.loading ? <LoaderCircle className="spin" /> : <textarea readOnly value={cell.value} spellCheck={false} />}
      <footer><span>{cell.value.length.toLocaleString()} 个字符</span><button className="button" onClick={() => { void navigator.clipboard.writeText(cell.value).then(() => setNotice("已复制完整内容")).catch(fail); }}>复制完整内容</button></footer>
    </section></div>}
    {parseOpen && <div className="modal-scrim"><section className="dialog settings-dialog"><header><h2>解析设置</h2><button className="icon-button" onClick={() => setParseOpen(false)}><X size={18} /></button></header><p className="muted">以下设置用于下一次打开文件。更改设置后重新打开会创建新的数据版本。</p>
      <label>文件编码<select value={parseSpec.encoding} onChange={e => setParseSpec(s => ({ ...s, encoding: e.target.value }))}><option value="auto">自动识别</option><option value="utf-8">UTF-8 / UTF-8 BOM</option><option value="gbk">GBK</option><option value="gb18030">GB18030（WHATWG 映射）</option></select></label>
      <label>分隔符<select value={parseSpec.delimiter ?? "auto"} onChange={e => setParseSpec(s => ({ ...s, delimiter: e.target.value === "auto" ? null : e.target.value }))}><option value="auto">自动识别</option><option value=",">逗号 ,</option><option value=";">分号 ;</option><option value={"\t"}>制表符 Tab</option><option value="|">竖线 |</option></select></label>
      <label className="check-line"><input type="checkbox" checked={parseSpec.hasHeaders} onChange={e => setParseSpec(s => ({ ...s, hasHeaders: e.target.checked }))} />第一条记录为表头</label>
      <div className="policy-note">保留空格与原文字段；空字段为空文本。列数不一致、非法编码或超出记录预算时停止并定位，不跳过。</div>
      <footer><span>记录上限 16MB · 字段上限 8MB</span><button className="button primary" onClick={() => setParseOpen(false)}>保存设置</button></footer>
    </section></div>}
    {exportDialog && current && <div className="modal-scrim"><section className="dialog settings-dialog"><header><h2>导出筛选结果</h2><button className="icon-button" onClick={() => setExportDialog(false)}><X size={18} /></button></header>
      <p className="muted">导出当前完整匹配的 {Number(query?.exactTotal ?? 0).toLocaleString()} 条记录，遵循当前排序。写出期间使用冻结的条件。</p>
      <label>列范围<select value={exportScope} onChange={e => setExportScope(e.target.value)}><option value="all">全部原始列（源顺序）</option><option value="visible">当前可见列（显示顺序）</option></select></label>
      <label>编码<select value={exportEncoding} onChange={e => setExportEncoding(e.target.value)}><option value="utf-8">UTF-8</option><option value="gbk">GBK</option><option value="gb18030">GB18030</option></select></label>
      <label className="check-line"><input type="checkbox" checked={exportBom} onChange={e => setExportBom(e.target.checked)} disabled={exportEncoding !== "utf-8"} />写入 UTF-8 BOM</label>
      <div className="policy-note">导出到新文件。只有完整写出并安全发布后才报告成功；不会覆盖源文件。</div>
      <footer><button className="button" onClick={() => setExportDialog(false)}>取消</button><button className="button primary" onClick={() => void startExport()}><Download size={15} />选择保存位置</button></footer>
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
