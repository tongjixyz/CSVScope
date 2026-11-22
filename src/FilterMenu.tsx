import { useEffect, useRef, useState } from "react";
import { Check, ChevronDown, Filter, LoaderCircle, Search, X } from "lucide-react";
import { api } from "./api";
import { emptySelection, isSelected, normalizeFilter, selectSearch, toggleValue, valueKey } from "./selection";
import type { Candidate, ColumnFilter, Predicate, Session, SortKey, ValueSelection } from "./types";

interface Props {
  tab: Session; column: number; onClose: () => void;
  onApply: (filter: ColumnFilter | null, sort?: SortKey[]) => Promise<void>;
}

export default function FilterMenu({ tab, column, onClose, onApply }: Props) {
  const initial = tab.view.filters[column];
  const [selection, setSelection] = useState<ValueSelection>(() => structuredClone(initial?.selection ?? emptySelection()));
  const [predicates, setPredicates] = useState<Predicate[]>(() => structuredClone(initial?.predicates ?? []));
  const [search, setSearch] = useState("");
  const [items, setItems] = useState<Candidate[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [cursor, setCursor] = useState("0");
  const [previous, setPrevious] = useState<string[]>([]);
  const [count, setCount] = useState<string | null>(null);
  const [status, setStatus] = useState("queued");
  const [partial, setPartial] = useState(false);
  const [sampleRefresh, setSampleRefresh] = useState(0);
  const [error, setError] = useState("");
  const [availability, setAvailability] = useState<Record<string, boolean | null>>({});
  const [mode, setMode] = useState("values");
  const predicate = initial?.predicates[0];
  const [operator, setOperator] = useState(predicate?.kind ?? "contains");
  const [value, setValue] = useState(predicate && "text" in predicate ? predicate.text : "");
  const [min, setMin] = useState(predicate && "min" in predicate ? predicate.min ?? "" : "");
  const [max, setMax] = useState(predicate && "max" in predicate ? predicate.max ?? "" : "");
  const [conditionDirty, setConditionDirty] = useState(false);
  const initialSort = tab.view.sort.find(s => s.column === column);
  const [sortDirection, setSortDirection] = useState(initialSort ? initialSort.descending ? "desc" : "asc" : "unchanged");
  const [sortInterpretation, setSortInterpretation] = useState<SortKey["interpretation"]>(initialSort?.interpretation ?? "text");
  const [busy, setBusy] = useState(false);
  const candidateId = useRef<string | null>(null);
  const generation = useRef(0);
  const baseRevision = useRef(tab.revision);
  const selectionRef = useRef(selection);
  selectionRef.current = selection;
  const ready = tab.status === "ready";
  const canQuery = ready || (tab.status === "preparing" && !!tab.query);
  const outdated = baseRevision.current !== tab.revision;

  useEffect(() => {
    const thisGeneration = ++generation.current;
    let timer: ReturnType<typeof setTimeout>;
    let disposed = false;
    setItems([]); setCount(null); setNext(null); setCursor("0"); setPrevious([]); setStatus("queued"); setError("");
    setPartial(!ready);
    setAvailability({});
    if (!canQuery || outdated) return;
    const poll = async (id: string) => {
      if (disposed || generation.current !== thisGeneration) return;
      try {
        const snapshot = await api.candidateStatus(id);
        if (disposed || generation.current !== thisGeneration) return;
        setStatus(snapshot.status);
        setPartial(snapshot.partial);
        if (snapshot.availability.length) setAvailability(Object.fromEntries(snapshot.availability.map(a => [valueKey(a.value), a.available])));
        if (snapshot.error) setError(snapshot.error);
        if (snapshot.status === "ready" || Number(snapshot.count) > 0) {
          const page = await api.candidatePage(id, "0");
          if (disposed || generation.current !== thisGeneration) return;
          setItems(page.items); setNext(page.next); setCount(page.exactTotal);
        }
        if (snapshot.status === "queued" || snapshot.status === "building") timer = setTimeout(() => void poll(id), 300);
      } catch (e) { if (!disposed) { setError(String(e)); setStatus("failed"); } }
    };
    timer = setTimeout(() => {
      void api.candidates(tab, column, search, selectionRef.current.rules.map(r => r.search),
        selectionRef.current.overrides.filter(p => p.selected).slice(0, 100).map(p => p.value)).then(id => {
        if (disposed) { void api.closeCandidates(id); return; }
        candidateId.current = id;
        void poll(id);
      }).catch(e => { if (!disposed) { setError(String(e)); setStatus("failed"); } });
    }, 250);
    return () => {
      disposed = true; clearTimeout(timer);
      if (candidateId.current) { void api.closeCandidates(candidateId.current); candidateId.current = null; }
    };
  }, [tab.id, tab.revision, column, search, ready, canQuery, outdated, sampleRefresh]); // Draft selection deliberately survives searches.

  useEffect(() => {
    const key = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onClose]);

  const loadPage = async (back = false) => {
    const target = back ? previous.at(-1) : next;
    const id = candidateId.current;
    const thisGeneration = generation.current;
    if (!id || !target || busy) return;
    setBusy(true);
    try {
      const page = await api.candidatePage(id, target);
      if (generation.current !== thisGeneration || candidateId.current !== id) return;
      setItems(page.items); setNext(page.next); setCount(page.exactTotal); setCursor(target);
      setPrevious(old => back ? old.slice(0, -1) : [...old, cursor]);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const apply = async () => {
    setBusy(true); setError("");
    try {
      let activePredicates = predicates;
      if (mode === "condition" && conditionDirty) {
        const predicate: Predicate = operator === "empty" || operator === "notEmpty"
          ? { kind: operator }
          : operator === "numericRange" || operator === "dateTimeRange"
          ? { kind: operator, min: min || null, max: max || null }
          : { kind: operator as "contains", text: value };
        activePredicates = [predicate];
      }
      const sort = sortDirection === "unchanged" ? undefined : sortDirection === "source" ? []
        : [{ column, descending: sortDirection === "desc", interpretation: sortInterpretation }];
      await onApply(normalizeFilter({ selection, predicates: activePredicates }), sort);
      onClose();
    } catch (e) { setError(String(e)); setBusy(false); }
  };
  const retained = selection.overrides.filter(point => point.selected &&
    (!items.some(item => valueKey(item.value) === valueKey(point.value)) || availability[valueKey(point.value)] === false));

  return <div className="modal-scrim" onMouseDown={e => { if (e.target === e.currentTarget) onClose(); }}>
    <section className="filter-menu" role="dialog" aria-label={`${tab.columns[column]?.name} 列筛选`}>
      <div className="filter-title"><span><Filter size={15} />{tab.columns[column]?.name}</span><button className="icon-button" onClick={onClose} aria-label="关闭筛选"><X size={17} /></button></div>
      <div className="sort-controls">
        <select aria-label="当前列排序" value={sortDirection} onChange={e => setSortDirection(e.target.value)}>
          <option value="unchanged">保持现有排序</option><option value="source">恢复源记录顺序</option>
          <option value="asc">此列升序</option><option value="desc">此列降序</option>
        </select>
        <select aria-label="排序解释" value={sortInterpretation} onChange={e => setSortInterpretation(e.target.value as SortKey["interpretation"])}>
          <option value="text">原始文本</option><option value="number">精确数值</option><option value="datetime">无时区日期时间</option>
        </select>
      </div>
      <div className="segmented">
        <button className={mode === "values" ? "selected" : ""} onClick={() => setMode("values")}>按值筛选</button>
        <button className={mode === "condition" ? "selected" : ""} onClick={() => setMode("condition")}>条件筛选</button>
      </div>
      {mode === "values" ? <>
        <div className="search-field"><Search size={15} /><input aria-label={ready ? "搜索完整候选集合" : "搜索已加载候选"} autoFocus placeholder={ready ? "搜索此列的所有候选值…" : "搜索已加载数据中的候选值…"} value={search} onChange={e => setSearch(e.target.value)} />{search && <button className="icon-button" onClick={() => setSearch("")}><X size={14} /></button>}</div>
        <div className="selection-actions">
          <button onClick={() => setSelection(emptySelection())}>全选</button><span>·</span>
          <button onClick={() => setSelection({ ...emptySelection(), defaultSelected: false })}>取消全选</button>
          {search && <button onClick={() => {
            setSelection(s => selectSearch(s, search));
            setItems(old => old.map(item => ({ ...item, matchingSearches: [...(item.matchingSearches ?? []), search] })));
          }}>全选搜索命中</button>}
        </div>
        {search && <p className="small-note">{ready ? "搜索覆盖完整候选集合。" : "当前搜索已加载部分。"}按规则全选后，后续读到的新命中也会选中。</p>}
        {partial && <p className="small-note">当前展示已加载部分的候选，最多 200 个，不代表全文件。完整加载后自动刷新。<button className="text-button" onClick={() => setSampleRefresh(n => n + 1)}>刷新已加载候选</button></p>}
        <div className="candidate-list">
          {!canQuery && <div className="candidate-state">首批数据正在准备，可先设置条件。</div>}
          {outdated && <div className="inline-error">筛选上下文已更新，草稿已保留。请取消后重新打开。</div>}
          {items.map((item, index) => <label className="candidate-row" key={`${valueKey(item.value)}-${index}`}>
            <input type="checkbox" checked={isSelected(selection, item)} onChange={() => setSelection(s => toggleValue(s, item))} />
            <span className={item.empty ? "muted italic" : ""} title={item.label}>{item.empty ? "(空字段)" : item.label}</span>
          </label>)}
          {(status === "queued" || status === "building") && canQuery && <div className="candidate-state"><LoaderCircle className="spin" size={16} />{status === "queued" ? "等待查询资源…" : partial ? "正在查询已加载候选…" : "正在查询完整候选集合…"}</div>}
          {status === "ready" && !items.length && <div className="candidate-state">{partial ? "已加载部分暂未找到候选，可刷新或直接设置条件" : "没有匹配的候选值"}</div>}
          {status === "ready" && (next || previous.length > 0) && <div className="selection-actions">
            <button onClick={() => void loadPage(true)} disabled={busy || previous.length === 0}>上一页</button>
            <span>第 {previous.length + 1} 页</span>
            <button onClick={() => void loadPage()} disabled={busy || !next}>下一页 <ChevronDown size={12} /></button>
          </div>}
        </div>
        {retained.length > 0 && <details className="unavailable" open={retained.some(p => availability[valueKey(p.value)] === false)}><summary>保留的显式已选值 {retained.length} 项</summary>
          {retained.slice(0, 100).map((point, index) => <div key={index}>{point.value.kind === "text" ? point.value.text || "(空字段)" : `记录 ${point.value.rid} 的完整值`}
            <span> · {availability[valueKey(point.value)] === false ? "当前其他列条件下不可用（保留条件）"
              : availability[valueKey(point.value)] === true ? "可用，当前页或搜索中未显示" : "可用状态待检查"}</span></div>)}
          {retained.length > 100 && <div>仅检查前 100 个显式值；其余选择继续保留。</div>}
        </details>}
        <div className="candidate-count">{partial ? `已列出 ${items.length} 个候选 · 完整数量待加载完成` : count === null ? "精确候选数量尚未完成" : `${Number(count).toLocaleString()} 个不同值`}</div>
      </> : <div className="condition-editor">
        <label>比较方式<select value={operator} onChange={e => { setOperator(e.target.value as Predicate["kind"]); setConditionDirty(true); }}>
          <option value="equals">等于</option><option value="notEquals">不等于</option>
          <option value="contains">包含</option><option value="notContains">不包含</option>
          <option value="numericRange">数值范围</option><option value="dateTimeRange">日期时间范围</option>
          <option value="empty">空字段</option><option value="notEmpty">非空字段</option>
        </select></label>
        {operator.endsWith("Range") ? <div className="range-inputs">
          <label>最小值（含）<input value={min} onChange={e => { setMin(e.target.value); setConditionDirty(true); }} placeholder={operator === "dateTimeRange" ? "2026-01-01 00:00:00" : "不限制"} /></label>
          <label>最大值（含）<input value={max} onChange={e => { setMax(e.target.value); setConditionDirty(true); }} placeholder="不限制" /></label>
        </div> : operator !== "empty" && operator !== "notEmpty" && <label>比较值<input value={value} onChange={e => { setValue(e.target.value); setConditionDirty(true); }} placeholder="按原始文本比较" /></label>}
        <p className="small-note">条件与此列的勾选项取 AND。数值采用 DECIMAL(38,9)，非法值不命中；日期首版按无时区时间比较。</p>
        <button className="text-button" onClick={() => setConditionDirty(true)}>使用当前条件</button>
        {predicates.length > 0 && <button className="text-button" onClick={() => { setPredicates([]); setConditionDirty(false); }}>清除现有高级条件（{predicates.length}）</button>}
      </div>}
      {error && <div className="inline-error">{error}</div>}
      <footer className="filter-footer">
        <button className="text-button danger-text" onClick={() => { void onApply(null).then(onClose).catch(e => setError(String(e))); }}>清除此列</button>
        <div><button className="button" onClick={onClose}>取消</button><button className="button primary" onClick={() => void apply()} disabled={busy || outdated}><Check size={14} />确定</button></div>
      </footer>
    </section>
  </div>;
}
