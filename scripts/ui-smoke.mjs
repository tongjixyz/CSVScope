import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";

const desktop = process.argv.includes("--desktop");
const output = path.resolve(".bench", desktop ? "desktop-smoke" : "browser-smoke");
await fs.mkdir(output, { recursive: true });
const browser = desktop
  ? await chromium.connectOverCDP(process.env.CSVSCOPE_TEST_CDP_URL ?? "http://127.0.0.1:9223")
  : await chromium.launch({ channel: "msedge", headless: true });
const context = desktop ? browser.contexts()[0] : await browser.newContext({ viewport: { width: 1440, height: 920 } });
const page = desktop ? context.pages()[0] : await context.newPage();
const errors = [];
page.on("pageerror", e => errors.push(String(e)));

try {
  if (!desktop) await page.goto("http://127.0.0.1:1420", { waitUntil: "networkidle" });
  await page.getByText("让大文件，回到轻松浏览").waitFor();
  await page.getByRole("button", { name: "解析设置" }).click();
  await page.getByRole("heading", { name: "解析设置", exact: true }).waitFor();
  await page.getByRole("button", { name: "保存设置" }).click();
  await page.screenshot({ path: path.join(output, "workspace.png") });
  if (desktop) {
    const fixture = path.resolve("fixtures", "linked.csv");
    const originalFile = await fs.readFile(fixture, "utf8");
    const tabId = await page.evaluate(async file => window.__TAURI_INTERNALS__.invoke("open_file", { path: file, spec: null }), fixture);
    const snapshot = async () => page.evaluate(id => window.__TAURI_INTERNALS__.invoke("session", { tabId: id }), tabId);
    const waitFor = async predicate => {
      const deadline = Date.now() + 20000;
      for (;;) {
        const s = await snapshot();
        assert.notEqual(s.status, "failed", JSON.stringify(s.error));
        assert.notEqual(s.query?.status, "failed", JSON.stringify(s.query));
        if (predicate(s)) {
          await page.waitForFunction(({ revision, status }) => {
            const grid = document.querySelector(".grid-shell");
            return grid?.getAttribute("data-revision") === String(revision)
              && grid.getAttribute("data-query-status") === status;
          }, { revision: s.revision, status: s.query?.status });
          return s;
        }
        assert.ok(Date.now() < deadline, JSON.stringify(s));
        await new Promise(resolve => setTimeout(resolve, 50));
      }
    };
    await waitFor(s => s.query?.status === "ready");
    await page.getByText("4 条结果 / 4 条记录").waitFor();
    const canvas = page.getByTestId("data-grid-canvas");
    const clickMenu = async x => {
      const box = await canvas.boundingBox();
      assert.ok(box, "table is visible");
      await page.mouse.move(box.x + x, box.y + 18);
      await page.mouse.click(box.x + x, box.y + 18);
    };
    await clickMenu(240);
    const menuA = page.getByRole("dialog", { name: "小区 列筛选" });
    await menuA.waitFor();
    await menuA.getByText("小区1", { exact: true }).waitFor();
    await menuA.getByRole("button", { name: "取消全选", exact: true }).click();
    await menuA.locator("label").filter({ hasText: "小区1" }).getByRole("checkbox").check();
    await menuA.getByRole("button", { name: "确定", exact: true }).click();
    await waitFor(s => s.query?.exactTotal === "2");
    await page.getByText("2 条结果 / 4 条记录").waitFor();
    await clickMenu(410);
    const menuB = page.getByRole("dialog", { name: "QCI 列筛选" });
    await menuB.waitFor();
    await menuB.locator(".candidate-count").getByText("2 个不同值").waitFor();
    assert.deepEqual(await menuB.locator(".candidate-row span").allTextContents(), ["1", "5"]);
    await page.screenshot({ path: path.join(output, "linked-candidates.png") });
    await menuB.getByRole("button", { name: "取消全选", exact: true }).click();
    await menuB.locator(".candidate-row").filter({ hasText: /^1$/ }).getByRole("checkbox").check();
    await menuB.getByRole("button", { name: "确定", exact: true }).click();
    await waitFor(s => s.query?.exactTotal === "1");
    await page.getByText("1 条结果 / 4 条记录").waitFor();
    await page.getByRole("button", { name: "撤销", exact: true }).click();
    await waitFor(s => s.query?.exactTotal === "2");
    await page.getByRole("button", { name: "重做", exact: true }).click();
    await waitFor(s => s.query?.exactTotal === "1");
    await page.getByText("1 条结果 / 4 条记录").waitFor();
    await page.screenshot({ path: path.join(output, "filtered-grid.png") });
    assert.equal((await snapshot()).view.filters["1"].selection.overrides[0].value.text, "1");
    await page.getByRole("button", { name: "清除全部", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && s.query.exactTotal === "4");
    await page.getByText("4 条结果 / 4 条记录").waitFor();
    const clickGrid = async (x, y, options = {}) => {
      const box = await canvas.boundingBox();
      assert.ok(box);
      await page.mouse.click(box.x + x, box.y + y, options);
    };
    const dataMenu = page.getByRole("menu", { name: "数据操作" });
    await page.evaluate(() => {
      window.__contextPrevented = false;
      document.addEventListener("contextmenu", event => { window.__contextPrevented = event.defaultPrevented; });
    });
    await clickGrid(165, 52, { button: "right" });
    await dataMenu.waitFor();
    assert.equal(await page.evaluate(() => window.__contextPrevented), true);
    assert.equal(await dataMenu.getByRole("menuitem", { name: "另存为 CSV…" }).count(), 1);
    assert.equal(await dataMenu.getByText(/刷新|打印|分享/).count(), 0);
    await page.screenshot({ path: path.join(output, "data-context-menu.png") });
    await dataMenu.getByRole("menuitem", { name: "编辑单元格", exact: true }).click();
    const draft = page.getByRole("textbox", { name: "单元格内容" });
    await draft.waitFor();
    await draft.fill("小区-编辑001");
    await page.getByRole("button", { name: "应用修改", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && s.view.edits?.cells["1"]?.["0"] === "小区-编辑001");
    await page.getByRole("button", { name: "关闭 linked.csv", exact: true }).click();
    await page.getByRole("dialog", { name: "关闭已修改文件" }).getByRole("button", { name: "取消", exact: true }).click();
    await clickGrid(165, 52, { button: "right" });
    await dataMenu.getByRole("menuitem", { name: "标记为黄色" }).click();
    await waitFor(s => s.view.edits?.colors["1"]?.["0"] === "#fff1b8");
    await page.getByRole("button", { name: "撤销", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && !s.view.edits?.colors["1"]);
    await page.getByRole("button", { name: "重做", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && s.view.edits?.colors["1"]?.["0"] === "#fff1b8");

    await clickGrid(165, 18);
    await page.keyboard.down("Shift");
    await clickGrid(470, 18);
    await page.keyboard.up("Shift");
    await clickGrid(300, 18, { button: "right" });
    await dataMenu.getByText("已选择 3 列", { exact: true }).waitFor();
    await page.keyboard.press("Escape");

    await clickGrid(300, 18);
    await page.keyboard.down("Control");
    await clickGrid(470, 18);
    await page.keyboard.up("Control");
    await clickGrid(300, 18, { button: "right" });
    await dataMenu.getByText("已选择 2 列", { exact: true }).waitFor();
    await dataMenu.getByRole("menuitem", { name: "隐藏所选列", exact: true }).click();
    await page.getByRole("button", { name: "列布局" }).click();
    assert.equal(await page.locator(".column-options label").filter({ hasText: /^QCI$/ }).getByRole("checkbox").isChecked(), false);
    assert.equal(await page.locator(".column-options label").filter({ hasText: /^IMSI$/ }).getByRole("checkbox").isChecked(), false);
    await page.screenshot({ path: path.join(output, "hidden-columns.png") });
    await page.getByRole("button", { name: "显示全部隐藏列", exact: true }).click();
    await page.getByRole("button", { name: "完成", exact: true }).click();

    await clickGrid(300, 18);
    await page.keyboard.down("Control");
    await clickGrid(470, 18);
    await page.keyboard.up("Control");
    await clickGrid(300, 18, { button: "right" });
    await dataMenu.getByRole("menuitem", { name: "删除所选列", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && s.view.edits?.deletedColumns.length === 2);
    assert.deepEqual((await snapshot()).view.edits.deletedColumns, [1, 2]);
    await page.getByRole("button", { name: "撤销", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && !s.view.edits?.deletedColumns.length);

    await clickGrid(40, 52);
    await page.keyboard.down("Shift");
    await clickGrid(40, 112);
    await page.keyboard.up("Shift");
    await clickGrid(40, 112, { button: "right" });
    await dataMenu.getByText("已选择 3 行", { exact: true }).waitFor();
    await page.keyboard.press("Escape");

    await clickGrid(40, 52);
    await page.keyboard.down("Control");
    await clickGrid(40, 112);
    await page.keyboard.up("Control");
    await clickGrid(40, 112, { button: "right" });
    await dataMenu.getByText("已选择 2 行", { exact: true }).waitFor();
    await dataMenu.getByRole("menuitem", { name: "删除所选行", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && s.query.exactTotal === "2" && s.view.edits?.deletedRows.length === 2);
    assert.deepEqual((await snapshot()).view.edits.deletedRows, ["1", "3"]);
    await page.getByRole("button", { name: "撤销", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && s.query.exactTotal === "4" && !s.view.edits?.deletedRows.length);

    await clickGrid(850, 52);
    await page.keyboard.press("Delete");
    await waitFor(s => s.query?.status === "ready" && s.view.edits?.cells["1"]?.["4"] === "");
    await page.getByRole("button", { name: "撤销", exact: true }).click();
    await waitFor(s => s.query?.status === "ready" && !Object.hasOwn(s.view.edits?.cells["1"] ?? {}, "4"));

    await clickGrid(675, 52);
    await page.evaluate(() => navigator.clipboard.writeText("2026-11-20 10:00:00\t粘贴,含逗号"));
    await page.keyboard.press("Control+v");
    await waitFor(s => s.query?.status === "ready" && s.view.edits?.cells["1"]?.["4"] === "粘贴,含逗号");
    await page.keyboard.press("Control+s");
    const saveDialog = page.getByRole("dialog", { name: "另存为 CSV", exact: true });
    await saveDialog.waitFor();
    assert.equal(await saveDialog.getByRole("combobox", { name: "保存行范围" }).inputValue(), "all");
    assert.equal(await saveDialog.getByRole("combobox", { name: "保存列范围" }).inputValue(), "all");
    const savedPath = path.join(output, `edited-${Date.now()}.csv`);
    await page.evaluate(file => {
      const original = window.__TAURI_INTERNALS__.invoke;
      window.__testOriginalInvoke = original;
      window.__TAURI_INTERNALS__.invoke = (command, args, options) => command === "plugin:dialog|save"
        ? Promise.resolve(file) : original(command, args, options);
    }, savedPath);
    await saveDialog.getByRole("button", { name: "选择保存位置", exact: true }).click();
    await saveDialog.waitFor({ state: "hidden" });
    await page.getByText(/CSV 已保存：/).waitFor();
    await page.evaluate(() => { window.__TAURI_INTERNALS__.invoke = window.__testOriginalInvoke; delete window.__testOriginalInvoke; });
    const savedText = await fs.readFile(savedPath, "utf8");
    assert.ok(savedText.includes("小区-编辑001,1,00123456789012345678,2026-11-20 10:00:00,\"粘贴,含逗号\""));
    assert.equal(await fs.readFile(fixture, "utf8"), originalFile);
    assert.ok(!(await page.locator(".file-tab.active").innerText()).includes("*"), "a complete saved copy clears the unsaved marker");
    const metadata = JSON.parse(await fs.readFile(`${savedPath}.csvscope.json`, "utf8"));
    assert.equal(metadata.colors["1"]["0"], "#fff1b8");
    const savedTab = await page.evaluate(file => window.__TAURI_INTERNALS__.invoke("open_file", { path: file, spec: null }), savedPath);
    await page.locator(".file-tab").filter({ hasText: path.basename(savedPath) }).waitFor();
    await page.locator(".file-tab").filter({ hasText: path.basename(savedPath) }).click();
    await page.waitForFunction(async id => {
      const state = await window.__TAURI_INTERNALS__.invoke("session", { tabId: id });
      return state.query?.status === "ready" && state.view.edits?.colors["1"]?.["0"] === "#fff1b8" && !state.modified;
    }, savedTab);
    await page.screenshot({ path: path.join(output, "edited-saved-reopened.png") });
    await page.evaluate(id => window.__TAURI_INTERNALS__.invoke("close_file", { tabId: id }), savedTab);
    await page.evaluate(async id => window.__TAURI_INTERNALS__.invoke("close_file", { tabId: id }), tabId);
  }
  assert.deepEqual(errors, [], errors.join("\n"));
  const result = { status: "passed", desktop, errors, screenshots: output, measuredAt: new Date().toISOString() };
  await fs.writeFile(path.join(output, "result.json"), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
} catch (error) {
  await page.screenshot({ path: path.join(output, "failure.png") }).catch(() => {});
  await fs.writeFile(path.join(output, "failure.json"), JSON.stringify({ error: String(error), errors, html: await page.content() }, null, 2));
  throw error;
} finally {
  if (!desktop) await browser.close();
  else await browser.close(); // CDP disconnects; the app lifetime is owned by the caller.
}
