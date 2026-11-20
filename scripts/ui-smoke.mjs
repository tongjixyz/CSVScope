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
    const tabId = await page.evaluate(async file => window.__TAURI_INTERNALS__.invoke("open_file", { path: file, spec: null }), fixture);
    const snapshot = async () => page.evaluate(id => window.__TAURI_INTERNALS__.invoke("session", { tabId: id }), tabId);
    const waitFor = async predicate => {
      const deadline = Date.now() + 20000;
      for (;;) {
        const s = await snapshot();
        assert.notEqual(s.status, "failed", JSON.stringify(s.error));
        assert.notEqual(s.query?.status, "failed", JSON.stringify(s.query));
        if (predicate(s)) return s;
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
