# installed-apps-scanner

Cross-platform installed-application scanner for **Node.js** and **Bun** — Windows / Linux / macOS, x64 & arm64.

Native [napi-rs](https://napi.rs) addon powered by the [`installed-apps`](https://crates.io/crates/installed-apps) Rust crate. Prebuilt binaries ship with the package — **no Rust toolchain required**.

- **Scan installed apps** — name, install path, launch command, publisher, version, PNG icon (base64, longest edge ≤ 256 px), and more
- **Match apps by file extension** — `appsForExtension("html")` lists every browser/editor that can open `.html`
- **Look up a single app by name** — `findApp("chrome")` returns its full info

> 中文文档与各平台扫描策略细节见仓库根目录 [README.md](https://github.com/jardenliu/installed-apps/blob/main/README.md)。

## Install

```bash
npm install installed-apps-scanner
# or
bun add installed-apps-scanner
```

The correct prebuilt binary is selected automatically via `optionalDependencies`:

| Platform package | Target |
| --- | --- |
| `installed-apps-scanner-win32-x64-msvc` | Windows x64 |
| `installed-apps-scanner-win32-arm64-msvc` | Windows arm64 |
| `installed-apps-scanner-darwin-x64` | macOS (Intel) |
| `installed-apps-scanner-darwin-arm64` | macOS (Apple Silicon) |
| `installed-apps-scanner-linux-x64-gnu` | Linux x64, glibc ≥ 2.17 |
| `installed-apps-scanner-linux-arm64-gnu` | Linux arm64, glibc ≥ 2.17 |

Requirements: Node.js ≥ 14 or Bun (Bun supports Node-API).

## Usage

```js
// ESM — Node.js and Bun
import binding from "installed-apps-scanner";
const { scan, appsForExtension, findApp } = binding;

// CJS
// const { scan, appsForExtension, findApp } = require("installed-apps-scanner");

// All installed applications
const apps = await scan();
// -> [{ id, name, installPath, executable, publisher, version,
//       launch: { kind: "command", program, args: ["... {file}"] },
//       icon: { base64, width, height },   // PNG base64, longest edge ≤ 256 px
//       extensions: ["html", ...], source: "uninstall_registry", ... }]

// Who can open .html?
const browsers = await appsForExtension("html");
// The leading dot is optional — appsForExtension(".html") is equivalent.

// Find one app by name (case-insensitive)
const chrome = await findApp("chrome"); // -> JsAppInfo | null
```

Opening a file with a matched app: replace the `"{file}"` placeholder in `launch.args` with the target path and run `launch.program`. On macOS `launch.kind === "mac_open"` — use `open -a <launch.appPath> <file>` instead (see [JsLaunchSpec](#jslaunchspec)).

All APIs are asynchronous and run on the libuv thread pool, so scanning never blocks the event loop.

## API

### `scan(): Promise<JsAppInfo[]>`

Scan every installed application the current platform exposes.

### `appsForExtension(ext: string): Promise<JsAppInfo[]>`

List applications registered to open `*.ext` files (dot optional), e.g. browsers and editors for `"html"`.

### `findApp(name: string): Promise<JsAppInfo | null>`

Find one application by name — case-insensitive; an exact match wins, otherwise the first partial match. Returns `null` when nothing matches (including empty-string input).

## Data model

### `JsAppInfo`

| Field | Type | Description |
| --- | --- | --- |
| `id` | `string` | Stable identifier (registry key / desktop-file id / bundle id) |
| `name` | `string` | Display name |
| `installPath` | `string` | Install location: executable, `.app` bundle, or `.desktop` file |
| `executable` | `string?` | Main executable path, when known |
| `publisher` | `string?` | Publisher / vendor |
| `version` | `string?` | Version string |
| `launch` | `JsLaunchSpec` | How the app opens a file |
| `icon` | `JsIcon?` | PNG icon (may be absent) |
| `extensions` | `string[]` | File extensions (no dot, lowercase) this app handles |
| `source` | `string` | Discovery source: `"uninstall_registry"` \| `"app_paths"` \| `"start_menu"` \| `"desktop_entry"` \| `"app_bundle"` \| `"file_association"` |

### `JsLaunchSpec`

| Field | Type | Description |
| --- | --- | --- |
| `kind` | `string` | `"command"` (direct launch) or `"mac_open"` (Launch Services) |
| `program` | `string?` | When `kind === "command"`: the executable to run |
| `args` | `string[]?` | When `kind === "command"`: arguments containing the `"{file}"` placeholder |
| `appPath` | `string?` | When `kind === "mac_open"`: the `.app` bundle path |

```js
// Launch pattern
if (app.launch.kind === "command") {
  spawn(app.launch.program, app.launch.args.map((a) => a.replace("{file}", targetPath)));
} else if (app.launch.kind === "mac_open") {
  spawn("open", ["-a", app.launch.appPath, targetPath]); // macOS
}
```

### `JsIcon`

| Field | Type | Description |
| --- | --- | --- |
| `base64` | `string` | Base64-encoded PNG bytes (no data-URI prefix) |
| `width` | `number` | Width in pixels |
| `height` | `number` | Height in pixels |

```js
const dataUri = `data:image/png;base64,${app.icon.base64}`;
```

## Notes & limitations

- **Windows**: covers the Uninstall registry (`HKLM`/`HKCU`, both 32- and 64-bit views), App Paths, and Start Menu shortcuts. **UWP / Microsoft Store apps are not included.** File associations honour the user's default (`UserChoice`) and fall back to `HKCR` ProgIDs.
- **Linux**: XDG `.desktop` entries (including flatpak / snap exports) and `mimeapps.list` / `mimeinfo.cache` associations; localized names (`Name[zh_CN]`) are supported.
- **macOS**: `.app` bundles under `/Applications`, `/System/Applications`, and `~/Applications`, with associations from `CFBundleDocumentTypes`.
- A single malformed entry is skipped; scanning never aborts partway.
- Icons are normalized to PNG with the longest edge ≤ 256 px (SVG and `.icns` are rasterized).

## Build from source

Only needed for development — the published package ships prebuilt binaries. Requires the [Rust toolchain](https://rustup.rs/).

```bash
git clone https://github.com/jardenliu/installed-apps.git
cd installed-apps/bindings/node
npm install        # installs @napi-rs/cli
npm run build      # produces installed-apps.<platform>-<arch>[-<libc>].node
npm test           # smoke test (also runs under Bun: bun test.mjs)
```

## License

MIT
