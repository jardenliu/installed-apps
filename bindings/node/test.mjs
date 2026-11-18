// Smoke test for the native binding. Runs under both Node.js and Bun:
//   node test.mjs
//   bun test.mjs
//
// Environment-tolerant: bare CI runners may have zero applications installed,
// so shape checks run only when results exist; API shape and error-free calls
// are asserted everywhere.
import assert from "node:assert/strict";
import binding from "./index.js";

const { scan, appsForExtension, findApp } = binding;

// --- scan() ---
const apps = await scan();
assert.ok(Array.isArray(apps), "scan() must return an array");
console.log(`scan(): ${apps.length} apps`);

if (apps.length > 0) {
  const sample = apps[0];
  for (const key of ["id", "name", "installPath", "launch", "extensions", "source"]) {
    assert.ok(key in sample, `missing field ${key}`);
  }
  assert.equal(typeof sample.launch.kind, "string");
  assert.ok(
    sample.launch.kind === "command" || sample.launch.kind === "mac_open",
    "launch kind must be valid"
  );
  assert.equal(typeof sample.installPath, "string");
  assert.ok(Array.isArray(sample.extensions));

  const withIcon = apps.find((a) => a.icon);
  if (withIcon) {
    // PNG magic bytes as base64
    assert.ok(withIcon.icon.base64.startsWith("iVBORw0KGgo"), "icon base64 must be PNG");
    assert.ok(withIcon.icon.width > 0 && withIcon.icon.height > 0);
    console.log(`icon: ${withIcon.icon.width}x${withIcon.icon.height} PNG OK`);
  }

  // --- findApp() round-trip ---
  const one = await findApp(sample.name);
  assert.ok(one, "findApp of a known name must hit");
  assert.equal(one.name.toLowerCase(), sample.name.toLowerCase());
  console.log(`findApp(${JSON.stringify(sample.name)}): ${one.name} OK`);
}

// --- appsForExtension() ---
const html = await appsForExtension("html");
assert.ok(Array.isArray(html), "appsForExtension must return an array");
for (const a of html) {
  assert.ok(a.launch.kind === "command" || a.launch.kind === "mac_open");
}
console.log(`appsForExtension("html"): ${html.length} handler(s)${html.length ? ` (${html.map((a) => a.name).join(", ")})` : ""}`);

const dotted = await appsForExtension(".html");
assert.equal(dotted.length, html.length, "leading dot must not matter");

// --- findApp() edge cases ---
assert.equal(await findApp(""), null, "empty name -> null");
assert.equal(await findApp("zzz_definitely_no_such_app_zzz"), null, "unknown name -> null");

console.log("all smoke tests passed");
