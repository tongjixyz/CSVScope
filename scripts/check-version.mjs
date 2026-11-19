import { appendFileSync, readFileSync } from "node:fs";

const workspace = new URL("../", import.meta.url);
const readJson = (path) =>
  JSON.parse(readFileSync(new URL(path, workspace), "utf8"));
const packageJson = readJson("package.json");
const packageLock = readJson("package-lock.json");
const tauri = readJson("src-tauri/tauri.conf.json");
const cargo = readFileSync(new URL("Cargo.toml", workspace), "utf8");
const workspacePackage = cargo.split("[workspace.package]")[1]?.split("\n[")[0];
const cargoVersion = workspacePackage?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
const version = packageJson.version;

for (const [file, actual] of [
  ["package-lock.json", packageLock.version],
  ["package-lock.json packages root", packageLock.packages?.[""]?.version],
  ["src-tauri/tauri.conf.json", tauri.version],
  ["Cargo.toml workspace.package", cargoVersion],
]) {
  if (actual !== version) {
    throw new Error(`${file}: expected version ${version}, found ${actual}`);
  }
}

const tag =
  process.argv[2] ??
  (process.env.GITHUB_REF_TYPE === "tag" ? process.env.GITHUB_REF_NAME : undefined);
if (tag !== undefined && tag !== `v${version}`) {
  throw new Error(`Release tag must be v${version}, found ${tag}`);
}

console.log(`Version ${version}${tag ? ` (${tag})` : ""} is consistent.`);
if (process.env.GITHUB_OUTPUT) {
  appendFileSync(process.env.GITHUB_OUTPUT, `version=${version}\n`, "utf8");
}
