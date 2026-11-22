import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";

const files = JSON.parse(process.env.CSVSCOPE_BENCH_FILES ?? "[]").map(p => path.resolve(p));
assert.ok(files.length > 0, "Pass -Files to scripts/desktop-smoke.ps1");
const output = process.env.CSVSCOPE_BENCH_OUTPUT ?? path.resolve(".bench/desktop-benchmark");
await fs.mkdir(output, { recursive: true });
const browser = await chromium.connectOverCDP(process.env.CSVSCOPE_TEST_CDP_URL ?? "http://127.0.0.1:9223");
const page = browser.contexts()[0].pages()[0];
const errors = [];
page.on("pageerror", e => errors.push(String(e)));
const invoke = (command, args = {}) => page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const started = performance.now();
const result = { status: "running", files, osCacheState: "uncontrolled", cases: [], notes: [
  "UI preview is observed through the grid accessibility value and two animation frames.",
  "Backend readiness is polled every 200ms; query completion numbers exclude human input time.",
  "Process-tree resource sampling is in process.json; Playwright's Node process is excluded.",
] };

async function state(tabId) { return invoke("session", { tabId }); }
async function waitReady(tabId) {
  const deadline = performance.now() + 60 * 60 * 1000;
  for (;;) {
    const s = await state(tabId);
    assert.ok(!["failed", "stale", "cancelled"].includes(s.status), JSON.stringify(s.error));
    assert.ok(!["failed", "cancelled"].includes(s.query?.status), JSON.stringify(s.query));
    if (s.query?.revision === s.revision && s.query.status === "ready") return s;
    assert.ok(performance.now() < deadline, "Task timed out");
    await pause(200);
  }
}
async function commit(tabId, view) {
  const before = await state(tabId);
  const start = performance.now();
  await invoke("commit_view", { tabId, revision: before.revision, view });
  const ready = await waitReady(tabId);
  return { seconds: (performance.now() - start) / 1000, rows: ready.query.exactTotal, state: ready };
}
function waitForGridValue(column, text) {
  return page.waitForFunction(({ column, text }) => Array.from(
    document.querySelectorAll(`[role="gridcell"][aria-colindex="${column}"]`)
  ).some(cell => cell.textContent === text), { column, text });
}

try {
  await page.getByText("让大文件，回到轻松浏览").waitFor();
  result.observedStartupSeconds = process.env.CSVSCOPE_TEST_START_MS
    ? (Date.now() - Number(process.env.CSVSCOPE_TEST_START_MS)) / 1000 : null;
  const opening = performance.now();
  const tabs = [];
  for (const file of files) tabs.push(await invoke("open_file", { path: file, spec: null }));
  await page.waitForFunction(() => document.querySelector('[data-testid="glide-cell-1-0"]')?.textContent?.length > 0, null, { timeout: 60000 });
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  result.firstRenderedDataSeconds = (performance.now() - opening) / 1000;
  await page.screenshot({ path: path.join(output, "preview.png") });
  if (tabs.length === 1) {
    const tabId = tabs[0];
    const loading = await state(tabId);
    if (loading.status === "preparing" && loading.query?.status === "loading" && Number(loading.query.generatedRows) > 1000) {
      await page.waitForFunction(() => {
        const status = document.querySelector(".status-left")?.textContent ?? "";
        const loaded = status.match(/已加载 ([\d,]+) 条/);
        return loaded && Number(loaded[1].replaceAll(",", "")) > 1000;
      });
      const target = 1000;
      const readStarted = performance.now();
      const copied = await invoke("copy_cells", {
        tabId, queryId: loading.query.id, start: String(target - 1), count: 1, columns: [0], includeRid: true,
      });
      assert.equal(copied[0][0], String(target));
      assert.equal(await invoke("full_cell", { tabId, rid: copied[0][0], column: 0 }), copied[0][1]);
      const jump = page.getByRole("textbox", { name: "跳转行号" });
      await jump.fill(String(target));
      await jump.press("Enter");
      await waitForGridValue(1, String(target));
      const after = await state(tabId);
      assert.equal(after.status, "preparing", "the loaded row must be usable before full preparation");
      assert.equal(after.query?.exactTotal, null);
      assert.ok(Number(after.query.generatedRows) >= Number(loading.query.generatedRows));
      result.progressiveLoading = {
        verified: true, target, availableRowsBefore: loading.query.generatedRows, availableRowsAfter: after.query.generatedRows,
        secondsFromOpening: (performance.now() - opening) / 1000, interactionSeconds: (performance.now() - readStarted) / 1000,
        operations: ["copy", "full field", "jump", "render beyond first 200 rows"],
      };
      await page.screenshot({ path: path.join(output, "background-loading.png") });
      const canvas = page.getByTestId("data-grid-canvas");
      const box = await canvas.boundingBox();
      assert.ok(box);
      await page.mouse.click(box.x + 240, box.y + 18);
      const menu = page.getByRole("dialog", { name: `${loading.columns[0].name} 列筛选` });
      await menu.waitFor();
      const text = await invoke("full_cell", { tabId, rid: "2", column: 0 });
      await menu.locator(".candidate-row").filter({ hasText: text }).first().waitFor();
      await menu.getByText(/当前展示已加载部分的候选/).waitFor();
      await page.screenshot({ path: path.join(output, "loading-candidates.png") });
      await menu.getByRole("button", { name: "条件筛选", exact: true }).click();
      await menu.getByLabel("比较方式").selectOption("equals");
      await menu.getByLabel("比较值").fill(text);
      const filtering = performance.now();
      await menu.getByRole("button", { name: "确定", exact: true }).click();
      await waitForGridValue(1, "2");
      const filtered = await state(tabId);
      assert.equal(filtered.status, "preparing", "filter results must render before EOF");
      assert.equal(filtered.query.status, "loading");
      assert.equal(filtered.query.exactTotal, null);
      assert.ok(Number(filtered.query.generatedRows) > 0);
      const sample = await invoke("copy_cells", {
        tabId, queryId: filtered.query.id, start: "0",
        count: Math.min(256, Number(filtered.query.generatedRows)), columns: [0], includeRid: true,
      });
      assert.ok(sample.every(row => row[1] === text));
      result.incrementalFilter = {
        verified: true, secondsToRenderedMatch: (performance.now() - filtering) / 1000,
        secondsFromOpening: (performance.now() - opening) / 1000,
        loadedSourceRows: filtered.rowsRead, scannedSourceRows: filtered.query.scannedRows,
        matchingRows: filtered.query.generatedRows, copiedRows: sample.length,
        queryId: filtered.query.id, preparing: true, exactTotal: filtered.query.exactTotal,
      };
      await page.screenshot({ path: path.join(output, "loading-filtered.png") });
    }
  }
  result.cases = await Promise.all(tabs.map(async (tabId, i) => {
    const s = await waitReady(tabId);
    return { file: files[i], bytes: s.totalBytes, rows: s.rowsRead, columns: s.columns.length,
      cacheHit: s.cacheHit, readyObservedSecondsFromOpening: (performance.now() - opening) / 1000 };
  }));
  result.cacheAfterAllReady = await invoke("cache_usage");
  if (result.incrementalFilter) {
    const complete = await state(tabs[0]);
    assert.equal(complete.query.id, result.incrementalFilter.queryId, "EOF must preserve the incremental index");
    assert.ok(Number(complete.query.exactTotal) > Number(result.incrementalFilter.matchingRows));
    result.incrementalFilter.finalMatchingRows = complete.query.exactTotal;
    result.incrementalFilter.sameQueryThroughEof = true;
  }
  if (tabs.length === 1) {
    const tabId = tabs[0];
    // The loading test left this same filter active. Reset first so the next
    // measurement builds a fresh full-file query rather than timing a no-op.
    if (result.incrementalFilter) await commit(tabId, { filters: {}, sort: [] });
    const text = await invoke("full_cell", { tabId, rid: "2", column: 0 });
    const view = { filters: { 0: { selection: null, predicates: [{ kind: "equals", text }] } }, sort: [] };
    const firstFilter = await commit(tabId, view);
    result.firstFilter = { seconds: firstFilter.seconds, rows: firstFilter.rows };
    const s = firstFilter.state;
    const start = performance.now();
    const candidateId = await invoke("open_candidates", { tabId, revision: s.revision, column: 1, search: "", searches: [], selectedValues: [] });
    for (;;) {
      const candidate = await invoke("candidate_status", { candidateId });
      assert.notEqual(candidate.status, "failed", candidate.error);
      if (candidate.status === "ready") {
        const candidatePage = await invoke("candidate_page", { candidateId, cursor: "0" });
        result.linkedCandidates = { seconds: (performance.now() - start) / 1000, count: candidate.count,
          values: candidatePage.items.map(i => i.label) };
        break;
      }
      await pause(50);
    }
    await invoke("close_candidates", { candidateId });
    const sorted = await commit(tabId, { filters: {}, sort: [{ column: 2, descending: true, interpretation: "text" }] });
    result.fullSort = { seconds: sorted.seconds, rows: sorted.rows };
    const total = Number(sorted.rows);
    await page.waitForFunction(rows => document.querySelector(".status-left")?.textContent?.includes(`${Number(rows).toLocaleString()} 条结果`), sorted.rows);
    const firstSortedRow = await invoke("copy_cells", { tabId, queryId: sorted.state.query.id, start: "0", count: 1, columns: [], includeRid: true });
    await waitForGridValue(1, firstSortedRow[0][0]);
    const target = Math.max(1, Math.floor(total * .9));
    const expected = await invoke("copy_cells", { tabId, queryId: sorted.state.query.id, start: String(target - 1), count: 1, columns: [], includeRid: true });
    const jump = page.getByRole("textbox", { name: "跳转行号" });
    const jumpStarted = performance.now();
    await jump.fill(String(target));
    await jump.press("Enter");
    result.farJump = { resultPosition: target, sourceRecord: expected[0][0], verified: false };
    await waitForGridValue(1, expected[0][0]);
    result.farJump = { resultPosition: target, sourceRecord: expected[0][0], seconds: (performance.now() - jumpStarted) / 1000, verified: true };
    result.cacheAtFullSort = await invoke("cache_usage");
    await page.screenshot({ path: path.join(output, "sorted-far-page.png") });
    if (s.columns.length >= 150) {
      const farColumn = 100;
      const expectedFrozen = await invoke("full_cell", { tabId, rid: expected[0][0], column: 0 });
      const expectedFar = await invoke("full_cell", { tabId, rid: expected[0][0], column: farColumn });
      await page.getByRole("button", { name: "列布局" }).click();
      await page.locator(".freeze-field select").selectOption("1");
      await page.getByRole("button", { name: "完成", exact: true }).click();
      const projecting = performance.now();
      await page.locator(".dvn-scroller").evaluate((element, offset) => { element.scrollLeft = offset; }, (farColumn - 1) * 170 + 5);
      await page.waitForFunction(({ rid, farColumn, expectedFrozen, expectedFar }) => {
        return Array.from(document.querySelectorAll('td[aria-colindex="1"]')).some(record => {
          const row = record.parentElement;
          return record.textContent === rid &&
            row?.querySelector('td[aria-colindex="2"]')?.textContent === expectedFrozen &&
            row?.querySelector(`td[aria-colindex="${farColumn + 2}"]`)?.textContent === expectedFar;
        });
      }, { rid: expected[0][0], farColumn, expectedFrozen, expectedFar }, { timeout: 60000 });
      const projected = await waitReady(tabId);
      assert.equal(projected.revision, sorted.state.revision);
      assert.ok(projected.query.projection.includes(farColumn));
      result.horizontalProjection = { seconds: (performance.now() - projecting) / 1000, frozenColumn: 0, farColumn, verified: true };
      await page.screenshot({ path: path.join(output, "wide-frozen-projection.png") });
    }
    const beforeCancel = await state(tabId);
    await invoke("commit_view", { tabId, revision: beforeCancel.revision,
      view: { filters: {}, sort: [{ column: 4, descending: true, interpretation: "text" }] } });
    await pause(20);
    const cancelling = performance.now();
    await invoke("cancel_task", { tabId });
    for (;;) {
      const s = await state(tabId);
      if (["ready", "cancelled", "failed"].includes(s.query?.status)) {
        result.cancel = { seconds: (performance.now() - cancelling) / 1000, finalStatus: s.query.status };
        break;
      }
      await pause(10);
    }
  } else {
    const first = page.locator(".file-tab").filter({ hasText: path.basename(files[0]) });
    const second = page.locator(".file-tab").filter({ hasText: path.basename(files[1]) });
    const start = performance.now();
    await first.click();
    await page.waitForFunction(rows => document.querySelector(".status-left")?.textContent?.includes(`${Number(rows).toLocaleString()} 条结果`), result.cases[0].rows);
    await waitForGridValue(1, "1");
    result.firstTabSwitchSeconds = (performance.now() - start) / 1000;
    await page.getByRole("textbox", { name: "跳转行号" }).fill("500000");
    await page.getByRole("textbox", { name: "跳转行号" }).press("Enter");
    await waitForGridValue(1, "500000");
    const text = await invoke("full_cell", { tabId: tabs[1], rid: "2", column: 0 });
    await second.click();
    await waitForGridValue(1, "1");
    const filtered = await commit(tabs[1], { filters: { 0: { selection: null, predicates: [{ kind: "equals", text }] } }, sort: [] });
    await page.waitForFunction(rows => document.querySelector(".status-left")?.textContent?.includes(`${Number(rows).toLocaleString()} 条结果`), filtered.rows);
    const restoring = performance.now();
    await first.click();
    await waitForGridValue(1, "500000");
    result.tabScrollRestoreSeconds = (performance.now() - restoring) / 1000;
    result.tabScrollRestore = "passed";
    assert.deepEqual((await state(tabs[0])).view, { filters: {}, sort: [] });
    assert.equal((await state(tabs[1])).query.exactTotal, filtered.rows);
    result.independentViews = "passed";
    await page.screenshot({ path: path.join(output, "five-tabs.png") });
  }
  assert.deepEqual(errors, [], errors.join("\n"));
  result.status = "passed";
  result.wallSeconds = (performance.now() - started) / 1000;
  result.errors = errors;
  await fs.writeFile(path.join(output, "result.json"), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
} catch (error) {
  result.status = "failed"; result.error = String(error); result.errors = errors;
  await page.screenshot({ path: path.join(output, "failure.png") }).catch(() => {});
  await fs.writeFile(path.join(output, "result.json"), JSON.stringify(result, null, 2));
  throw error;
} finally { await browser.close(); }
