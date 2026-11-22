import { readFileSync, writeFileSync } from "node:fs";

const root = new URL("../", import.meta.url);
const read = path => readFileSync(new URL(path, root), "utf8");
const pkg = JSON.parse(read("package.json"));
const current = pkg.version;
const requested = process.argv[2];
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?$/;
if (!semver.test(current)) throw new Error(`Invalid current version: ${current}`);
let next = requested;
if (["major", "minor", "patch"].includes(requested)) {
  const [major, minor, patch] = current.split("-")[0].split(".").map(Number);
  next = requested === "major" ? `${major + 1}.0.0` : requested === "minor" ? `${major}.${minor + 1}.0` : `${major}.${minor}.${patch + 1}`;
}
if (!next || !semver.test(next) || next === current) {
  throw new Error("Usage: npm run version:bump -- patch|minor|major|<version> [--dry-run]");
}
const lock = JSON.parse(read("package-lock.json"));
const tauri = JSON.parse(read("src-tauri/tauri.conf.json"));
pkg.version = next;
lock.version = next;
lock.packages[""].version = next;
tauri.version = next;
const cargo = read("Cargo.toml").replace(/(\[workspace\.package\][\s\S]*?\bversion\s*=\s*")[^"]+(")/, `$1${next}$2`);
if (!cargo.includes(`version = "${next}"`)) throw new Error("Workspace Cargo version was not found");
let replaced = 0;
const cargoLock = read("Cargo.lock").replace(/(\[\[package\]\]\r?\nname = "(?:csvscope|csvscope-core)"\r?\nversion = ")[^"]+(")/g, (_match, before, after) => {
  replaced++;
  return `${before}${next}${after}`;
});
if (replaced !== 2) throw new Error(`Expected two workspace packages in Cargo.lock, found ${replaced}`);
const json = value => JSON.stringify(value, null, 2) + "\n";
const updates = [
  ["package.json", json(pkg)], ["package-lock.json", json(lock)],
  ["Cargo.toml", cargo], ["Cargo.lock", cargoLock], ["src-tauri/tauri.conf.json", json(tauri)],
];
if (process.argv.includes("--dry-run")) console.log(`Would bump ${current} -> ${next}: ${updates.map(([path]) => path).join(", ")}`);
else {
  for (const [path, contents] of updates) writeFileSync(new URL(path, root), contents, "utf8");
  console.log(`Bumped ${current} -> ${next}. Update CHANGELOG.md, run npm run check:version and scripts/test.ps1, then commit and tag v${next}.`);
}
