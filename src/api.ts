import { invoke, isTauri } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { CacheUsage, CandidatePage, CandidateStatus, ExportStatus, FilterValue, ParseSpec, Session, ViewState } from "./types";

export const native = isTauri();
export const api = {
  sessions: () => invoke<Session[]>("sessions"),
  session: (tabId: string) => invoke<Session>("session", { tabId }),
  open: (path: string, spec: ParseSpec) => invoke<string>("open_file", { path, spec }),
  commit: (tab: Session, view: ViewState) => invoke<Session>("commit_view", { tabId: tab.id, revision: tab.revision, view }),
  history: (tab: Session, redo: boolean) => invoke<Session>("history_action", { tabId: tab.id, revision: tab.revision, redo }),
  projection: (tabId: string, revision: number, columns: number[]) => invoke<string>("request_projection", { tabId, revision, columns }),
  page: async (tabId: string, queryId: string, start: number, columns: number[]) => {
    const bytes = await invoke<ArrayBuffer>("read_page", { tabId, queryId, start: String(start), count: 256, columns });
    const { tableFromIPC } = await import("apache-arrow");
    return tableFromIPC(new Uint8Array(bytes));
  },
  cell: (tabId: string, rid: string, column: number) => invoke<string>("full_cell", { tabId, rid, column }),
  copy: (tabId: string, queryId: string, start: number, count: number, columns: number[], includeRid: boolean) =>
    invoke<string[][]>("copy_cells", { tabId, queryId, start: String(start), count, columns, includeRid }),
  candidates: (tab: Session, column: number, search: string, searches: string[] = [], selectedValues: FilterValue[] = []) =>
    invoke<string>("open_candidates", { tabId: tab.id, revision: tab.revision, column, search, searches, selectedValues }),
  candidateStatus: (candidateId: string) => invoke<CandidateStatus>("candidate_status", { candidateId }),
  candidatePage: (candidateId: string, cursor: string) => invoke<CandidatePage>("candidate_page", { candidateId, cursor }),
  closeCandidates: (candidateId: string) => invoke<void>("close_candidates", { candidateId }),
  cancel: (tabId: string) => invoke<void>("cancel_task", { tabId }),
  close: (tabId: string) => invoke<void>("close_file", { tabId }),
  activate: (tabId: string) => invoke<void>("activate_file", { tabId }),
  export: (tab: Session, path: string, columns: number[], bom: boolean, encoding: string) => invoke<string>("start_export", {
    tabId: tab.id, queryId: tab.query!.id, spec: {
      path, columns, delimiter: tab.parseSpec.delimiter ?? ",", encoding, bom, headers: tab.parseSpec.hasHeaders,
    },
  }),
  exportStatus: (exportId: string) => invoke<ExportStatus>("export_status", { exportId }),
  cancelExport: (exportId: string) => invoke<void>("cancel_export", { exportId }),
  usage: () => invoke<CacheUsage>("cache_usage"),
  clearCache: () => invoke<string>("clear_cache"),
};

export async function chooseFiles(): Promise<string[]> {
  if (!native) return [];
  const paths = await open({ multiple: true, directory: false, filters: [
    { name: "CSV / TSV / 日志数据", extensions: ["csv", "tsv", "txt", "log"] },
    { name: "所有文件", extensions: ["*"] },
  ] });
  return paths ? (Array.isArray(paths) ? paths : [paths]) : [];
}
export async function chooseExport(name: string): Promise<string | null> {
  return save({ defaultPath: name.replace(/\.[^.]+$/, "") + "-filtered.csv", filters: [{ name: "CSV", extensions: ["csv"] }] });
}
