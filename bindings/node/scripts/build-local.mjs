// Local multi-target build helper.
//
// Builds the native binding for every target this host *can* produce and
// places the artifacts next to index.js using the same file names the CI
// workflow produces (so the generated loader picks them up):
//
//   win32-x64-msvc      native on an x64 Windows host
//   win32-arm64-msvc    cross: needs rustup target + VS Build Tools (ARM64)
//   linux-x64-gnu       cross: needs zig + cargo-zigbuild (glibc 2.17 baseline)
//   linux-arm64-gnu     cross: needs zig + cargo-zigbuild (glibc 2.17 baseline)
//   darwin-*            impossible from Windows — use macOS or CI
//
// Usage:
//   node scripts/build-local.mjs            # everything possible
//   node scripts/build-local.mjs x86_64-pc-windows-msvc   # selected targets
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const pkgDir = join(scriptsDir, "..");
const repoRoot = join(pkgDir, "..", "..");

// Explicit glibc baseline for Linux targets. The `.2.17` suffix passed to
// cargo-zigbuild is REQUIRED: without it the artifacts pick up GLIBC_2.29+
// symbol versions and fail to load on glibc 2.28 systems (Kylin V10 / UOS).
const LINUX_GLIBC = "2.17";

const TARGETS = [
  {
    triple: "x86_64-pc-windows-msvc",
    out: "installed-apps.win32-x64-msvc.node",
    artifact: "installed_apps_node.dll",
    kind: "native-win",
  },
  {
    triple: "aarch64-pc-windows-msvc",
    out: "installed-apps.win32-arm64-msvc.node",
    artifact: "installed_apps_node.dll",
    kind: "cross-win",
  },
  {
    triple: "x86_64-unknown-linux-gnu",
    out: "installed-apps.linux-x64-gnu.node",
    artifact: "libinstalled_apps_node.so",
    kind: "zig",
  },
  {
    triple: "aarch64-unknown-linux-gnu",
    out: "installed-apps.linux-arm64-gnu.node",
    artifact: "libinstalled_apps_node.so",
    kind: "zig",
  },
];

function run(cmd, args, opts = {}) {
  const res = spawnSync(cmd, args, {
    cwd: repoRoot,
    stdio: "inherit",
    ...opts,
  });
  return res.status === 0;
}

function capture(cmd, args) {
  const res = spawnSync(cmd, args, {
    cwd: repoRoot,
    encoding: "utf8",
  });
  return (res.stdout || "") + (res.stderr || "");
}

function installedTargets() {
  return capture("rustup", ["target", "list", "--installed"])
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function hasTool(cmd, args) {
  return spawnSync(cmd, args).status === 0;
}

const requested = process.argv.slice(2);
const results = [];

for (const t of TARGETS) {
  if (requested.length > 0 && !requested.includes(t.triple)) {
    continue;
  }

  // macOS artifacts can only be produced on macOS.
  if (process.platform !== "darwin" && t.triple.includes("apple")) {
    results.push({ target: t.triple, status: "skip", note: "requires macOS (use CI)" });
    continue;
  }

  if (t.kind === "zig" && !hasTool("cargo", ["zigbuild", "--version"])) {
    results.push({
      target: t.triple,
      status: "skip",
      note: "needs `pip install ziglang` + `cargo install cargo-zigbuild`",
    });
    continue;
  }

  if (!installedTargets().includes(t.triple)) {
    const added = run("rustup", ["target", "add", t.triple]);
    if (!added) {
      results.push({ target: t.triple, status: "skip", note: "rustup target add failed" });
      continue;
    }
  }

  const buildOk =
    t.kind === "zig"
      ? run("cargo", [
          "zigbuild",
          "--target",
          `${t.triple}.${LINUX_GLIBC}`,
          "-p",
          "installed-apps-node",
          "--release",
        ])
      : run("cargo", ["build", "--target", t.triple, "-p", "installed-apps-node", "--release"]);
  if (!buildOk) {
    const note =
      t.kind === "cross-win"
        ? "build failed (cross to win-arm64 needs VS Build Tools with the ARM64 component)"
        : "build failed";
    results.push({ target: t.triple, status: "fail", note });
    continue;
  }

  const built = join(repoRoot, "target", t.triple, "release", t.artifact);
  if (!existsSync(built)) {
    results.push({ target: t.triple, status: "fail", note: `artifact missing: ${built}` });
    continue;
  }
  mkdirSync(pkgDir, { recursive: true });
  copyFileSync(built, join(pkgDir, t.out));
  results.push({ target: t.triple, status: "ok", note: t.out });
}

console.log("\n=== build-local summary ===");
for (const r of results) {
  console.log(`  [${r.status}] ${r.target}  ${r.note}`);
}
if (results.some((r) => r.status === "ok")) {
  console.log("\nRun `node test.mjs` to smoke-test the host-architecture binding.");
}
console.log("darwin targets are not buildable from this host — use the CI workflow.");
