import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";

const output = path.resolve(".bench/diagnostics-smoke");
await fs.mkdir(output, { recursive: true });
const browser = await chromium.connectOverCDP(process.env.CSVSCOPE_TEST_CDP_URL ?? "http://127.0.0.1:9223");
const page = browser.contexts()[0].pages()[0];
const invoke = (command, args = {}) => page.evaluate(({ command, args }) =>
  window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const started = Date.now();
let logDirectory;

async function events() {
  const files = (await fs.readdir(logDirectory)).filter(name => /^csvscope-.*\.log$/.test(name));
  const result = [];
  for (const file of files) {
    const lines = (await fs.readFile(path.join(logDirectory, file), "utf8")).trim().split("\n");
    for (const line of lines) {
      try { result.push(JSON.parse(line)); } catch { /* A final line can still be in flight. */ }
    }
  }
  return result.filter(event => event.timestampUnixMs >= started);
}

try {
  await page.getByRole("button", { name: "打开运行日志" }).waitFor();
  const info = await invoke("diagnostic_info");
  assert.ok(info.directory);
  logDirectory = info.directory;
  await assert.rejects(invoke("open_file", { path: path.resolve(".bench/missing-diagnostics-file.csv"), spec: null }));
  await page.evaluate(() => {
    window.dispatchEvent(new ErrorEvent("error", {
      message: "diagnostic-smoke-frontend", filename: "diagnostics-smoke",
      error: new Error("diagnostic-smoke-frontend"),
    }));
  });
  // Block only this test instance's renderer. The independent native watchdog must still log it.
  await page.evaluate(async () => {
    await window.__TAURI_INTERNALS__.invoke("frontend_heartbeat", { visible: true, pending: [] });
    const deadline = performance.now() + 12500;
    while (performance.now() < deadline) { /* Deliberately simulate an event-loop stall. */ }
  });
  await invoke("frontend_heartbeat", { visible: true, pending: [] });
  let recorded;
  const deadline = Date.now() + 12000;
  for (;;) {
    recorded = await events();
    if (recorded.some(e => e.event === "ui_recovered")) break;
    assert.ok(Date.now() < deadline, "Watchdog recovery event was not written");
    await pause(250);
  }
  const hang = recorded.find(e => e.event === "ui_unresponsive");
  assert.ok(hang, "Watchdog must record a stalled renderer");
  assert.ok(hang.details.frontendHeartbeatGapMs >= 10000);
  assert.ok(hang.details.nativeHeartbeatGapMs < 5000, "Native UI loop must remain responsive");
  assert.ok(recorded.some(e => e.event === "frontend_event_loop_delay"));
  assert.ok(recorded.some(e => e.event === "frontend_error" && e.details.message === "diagnostic-smoke-frontend"));
  assert.ok(recorded.some(e => e.event === "operation_finished" && e.details.operation === "open_file" && e.details.outcome === "error"));
  const result = {
    status: "passed", logDirectory, simulatedRendererStallMs: 12500,
    verified: ["operation errors", "frontend errors", "independent stall detection", "native responsiveness", "recovery", "event-loop delay"],
    seconds: (Date.now() - started) / 1000,
    measuredAt: new Date().toISOString(),
  };
  await fs.writeFile(path.join(output, "result.json"), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
} catch (error) {
  await fs.writeFile(path.join(output, "failure.json"), JSON.stringify({ error: String(error), logDirectory }, null, 2));
  throw error;
} finally { await browser.close(); }
