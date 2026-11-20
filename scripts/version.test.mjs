import { afterEach, expect, it } from "vitest";
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync, copyFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const fixtures = [];
afterEach(() => {
  for (const directory of fixtures.splice(0)) {
    if (path.dirname(path.resolve(directory)) !== path.resolve(tmpdir()) || !path.basename(directory).startsWith("csvscope-version-")) {
      throw new Error(`Refusing to remove unexpected fixture directory: ${directory}`);
    }
    rmSync(directory, { recursive: true, force: true });
  }
});
function fixture() {
  const directory = mkdtempSync(path.join(tmpdir(), "csvscope-version-"));
  fixtures.push(directory);
  mkdirSync(path.join(directory, "scripts"));
  mkdirSync(path.join(directory, "src-tauri"));
  for (const file of ["bump-version.mjs", "check-version.mjs"]) copyFileSync(new URL(file, import.meta.url), path.join(directory, "scripts", file));
  const write = (file, data) => writeFileSync(path.join(directory, file), typeof data === "string" ? data : JSON.stringify(data));
  write("package.json", { name: "csvscope", version: "2.3.4" });
  write("package-lock.json", { version: "2.3.4", packages: { "": { version: "2.3.4" }, "node_modules/example": { version: "9.9.9" } } });
  write("src-tauri/tauri.conf.json", { version: "2.3.4", productName: "CSVScope" });
  write("Cargo.toml", '[workspace.package]\nversion = "2.3.4"\nedition = "2024"\n');
  write("Cargo.lock", 'version = 4\n[[package]]\nname = "csvscope"\nversion = "2.3.4"\n[[package]]\nname = "csvscope-core"\nversion = "2.3.4"\n[[package]]\nname = "example"\nversion = "9.9.9"\n');
  write("CHANGELOG.md", "## [2.3.4] - 2026-11-20\n");
  return { directory, write, read: file => readFileSync(path.join(directory, file), "utf8"),
    run: (script, ...args) => spawnSync(process.execPath, [path.join(directory, "scripts", script), ...args], {
      encoding: "utf8", env: { ...process.env, GITHUB_OUTPUT: "", GITHUB_REF_TYPE: "", GITHUB_REF_NAME: "" },
    }) };
}

it("bumps every product version while preserving third-party lock versions", () => {
  const f = fixture();
  expect(f.run("bump-version.mjs", "minor").status).toBe(0);
  expect(JSON.parse(f.read("package.json")).version).toBe("2.4.0");
  expect(JSON.parse(f.read("package-lock.json")).packages[""].version).toBe("2.4.0");
  expect(JSON.parse(f.read("package-lock.json")).packages["node_modules/example"].version).toBe("9.9.9");
  expect(JSON.parse(f.read("src-tauri/tauri.conf.json")).version).toBe("2.4.0");
  expect(f.read("Cargo.toml")).toContain('version = "2.4.0"');
  expect(f.read("Cargo.lock").match(/version = "2\.4\.0"/g)).toHaveLength(2);
  expect(f.read("Cargo.lock")).toContain('name = "example"\nversion = "9.9.9"');
  expect(f.run("check-version.mjs").status).not.toBe(0);
  f.write("CHANGELOG.md", "## [2.4.0] - 2026-11-20\n");
  expect(f.run("check-version.mjs", "v2.4.0").status).toBe(0);
});

it("rejects invalid and identical versions and makes dry-run read only", () => {
  const f = fixture();
  const original = f.read("package.json");
  for (const input of ["2.3.4", "garbage", "01.2.3", "2.4.0-beta.01"]) expect(f.run("bump-version.mjs", input).status).not.toBe(0);
  expect(f.run("bump-version.mjs", "2.4.0-beta.1", "--dry-run").status).toBe(0);
  expect(f.read("package.json")).toBe(original);
});

it("rejects a mismatched release tag or Rust lock version", () => {
  const f = fixture();
  expect(f.run("check-version.mjs", "v2.3.4").status).toBe(0);
  expect(f.run("check-version.mjs", "v2.3.5").status).not.toBe(0);
  f.write("Cargo.lock", f.read("Cargo.lock").replace('name = "csvscope-core"\nversion = "2.3.4"', 'name = "csvscope-core"\nversion = "2.3.3"'));
  expect(f.run("check-version.mjs").status).not.toBe(0);
});
