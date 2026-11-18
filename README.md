# installed-apps

跨平台已安装应用扫描库(Rust):**Windows / Linux / macOS**,兼容 **x64 与 arm64**。

核心能力:

1. **扫描已安装应用** —— 返回名称、安装路径、打开文件的方式、图标(PNG base64,最长边 ≤ 256px)等;
2. **按文件扩展名匹配应用** —— 例如 `html` 可以列出所有浏览器 / 编辑器;
3. **按名称查询单个应用** —— 返回该应用的完整信息。

## 快速开始

```rust
use installed_apps::Scanner;

let scanner = Scanner::new();

// 全部应用
for app in scanner.scan()? {
    println!("{} -> {}", app.name, app.launch.template());
}

// 谁能打开 .html?
for app in scanner.apps_for_extension("html")? {
    println!("{}", app.name);
    if let Some(icon) = &app.icon {
        // data:image/png;base64,...
        println!("{}", icon.data_uri());
    }
}

// 查询单个应用
let chrome = scanner.find_app("chrome")?;
```

### 示例 CLI

```bash
cargo run --bin ia -- list             # 列出全部应用
cargo run --bin ia -- assoc html       # 谁能打开 .html
cargo run --bin ia -- app chrome       # 按名称查询
cargo run --bin ia -- assoc pdf --json # JSON 输出(含完整图标 base64)
```

## 数据模型

`AppInfo` 字段:

| 字段 | 说明 |
| --- | --- |
| `id` | 稳定标识(注册表键 / desktop 文件 id / Bundle ID) |
| `name` | 显示名称 |
| `install_path` | 安装位置:exe / `.app` bundle / `.desktop` 文件 |
| `executable` | 主可执行文件路径(已解析) |
| `publisher` / `version` | 发布者 / 版本(可得时) |
| `launch` | **打开文件的方式**,`LaunchSpec::Command { program, args }`(args 中 `{file}` 为文件占位符)或 macOS 的 `LaunchSpec::MacOpen`;用 `launch.build_command(path)` 直接得到 `std::process::Command`,用 `launch.template()` 得到可读模板 |
| `icon` | **PNG base64**(最长边 ≤ 256px),`icon.base64` / `icon.data_uri()` |
| `extensions` | 该应用注册支持的扩展名 |
| `source` | 发现来源(卸载注册表 / App Paths / 开始菜单 / desktop 文件 / app bundle / 文件关联) |

所有类型均实现 `serde::Serialize`,可直接序列化为 JSON。

## 各平台扫描策略

### Windows(不含 UWP / Store 应用)

- **应用枚举**:卸载注册表 `HKLM/HKCU\...\Uninstall`(显式读 32 位与 64 位两个视图,arm64/x64 进程均可看全)、`App Paths`、开始菜单 `*.lnk` 快捷方式;按路径去重,过滤更新包 / 系统组件。
- **文件关联**:`HKCU\...\FileExts\.ext\UserChoice`(用户默认)+ `HKCR\.ext` 默认 ProgId + `OpenWithProgids`,解析 `shell\open\command` 命令模板(`%1` → `{file}`)。命中的应用会用**真实关联模板**覆盖通用启动命令。
- **图标**:`ExtractIconExW`(支持 `path,index` 语法)→ HICON → BGRA → PNG base64;含老式图标 alpha 修复(AND mask 回退、预乘 alpha 还原)。

### Linux

- **应用枚举**:XDG `.desktop` 目录(`~/.local/share/applications`、`/usr/local/share`、`/usr/share`)+ flatpak / snap 导出目录;解析 `Name/Exec/Icon/MimeType` 等,支持本地化 `Name[zh_CN]`。
- **文件关联**:扩展名 → MIME(解析 `/usr/share/mime/globs2`,回退内置常用表)→ `mimeapps.list`(`[Default Applications]` / `[Added Associations]`,用户配置优先于系统)+ `mimeinfo.cache`;`Exec` 字段码 `%f/%F/%u/%U` → `{file}`,`%i` 丢弃,`%%` 转义。
- **图标**:icon theme 查找(hicolor/Adwaita/Yaru/breeze/Papirus,pixmaps 回退);SVG 用 `resvg` 栅格化;统一转 PNG ≤ 256px。

### macOS

- **应用枚举**:`/Applications`、`/System/Applications`(含 Utilities)、`~/Applications` 下的 `.app` bundle;解析 `Info.plist`(`CFBundleName`、`CFBundleIdentifier`、`CFBundleShortVersionString`、`CFBundleExecutable`)。
- **文件关联**:`CFBundleDocumentTypes` 的 `CFBundleTypeExtensions`,`LSItemContentTypes` 经内置常见 UTI→扩展名表补充。
- **打开方式**:`LaunchSpec::MacOpen`(`open -a <bundle> <file>`)。
- **图标**:`Contents/Resources/*.icns` 解码(`icns` crate),取最大尺寸表示,转 PNG ≤ 256px。

## 兼容性说明

- **x64 / arm64**:无架构专属代码;Windows 通过双注册表视图覆盖 32/64 位应用,macOS/Linux 目录扫描天然兼容。
- Windows 平台**不包含 UWP / Microsoft Store 应用**(如"照片");Chrome、Firefox、Edge 桌面版等传统应用均可覆盖。
- 单条记录解析失败会被跳过,不会中断整体扫描。

## 在 Node.js / Bun 中使用

通过 `bindings/node` 的 [napi-rs](https://napi.rs) 原生扩展调用,**Node.js 与 Bun 通用**(Bun 兼容 Node-API),异步接口运行在 libuv 线程池、不阻塞事件循环。TypeScript 类型开箱即用。

### 构建(需要 Rust 工具链)

```bash
cd bindings/node
npm install        # 安装 @napi-rs/cli
npm run build      # 产出 installed-apps.<platform>-<arch>[-<libc>].node
node test.mjs      # 冒烟测试(或 bun test.mjs)
```

### 用法

```js
// Node.js (CJS/ESM) 与 Bun 均可
import binding from "installed-apps-scanner"; // 本地: import binding from "./bindings/node/index.js"
const { scan, appsForExtension, findApp } = binding;

const apps = await scan();
// -> [{ id, name, installPath, executable, publisher, version,
//       launch: { kind: "command", program, args: ["... {file}"] },
//       icon: { base64, width, height },   // PNG base64,最长边 ≤256px
//       extensions: ["html", ...], source: "uninstall_registry", ... }]

const browsers = await appsForExtension("html");   // 谁能打开 .html
const chrome = await findApp("chrome");            // 按名称查询,无结果返回 null

// 打开文件:把 launch.args 里的 {file} 替换为目标路径
// (macOS 为 launch.kind === "mac_open",用 `open -a <appPath> <file>`)
```

完整类型定义见 [bindings/node/index.d.ts](bindings/node/index.d.ts)。

> npm 发布布局已按 napi-rs 标准预留(`package.json` 的 `napi.targets`),将来发布 prebuilt 多平台包只需补 CI 构建。

## 多平台构建与发布

6 个目标:win32-x64 / win32-arm64 / darwin-x64 / darwin-arm64 / linux-x64-gnu / linux-arm64-gnu。

### CI(推荐):GitHub Actions

[.github/workflows/build.yml](.github/workflows/build.yml) 在 push/PR 时构建全部目标并逐平台冒烟,推送 `v*` tag 自动发布到 npm:

| runner | 目标 | 说明 |
|---|---|---|
| windows-latest | win32-x64-msvc | 原生构建 + 冒烟 |
| windows-11-arm | win32-arm64-msvc | 原生构建 + 冒烟 |
| macos-latest | darwin-arm64 / darwin-x64 | x64 为 Apple 交叉编译(仅 arm64 冒烟) |
| ubuntu-latest | linux-x64-gnu | **cargo-zigbuild**,glibc ≥ 2.17 |
| ubuntu-24.04-arm | linux-arm64-gnu | cargo-zigbuild + 冒烟 |

- **Linux 兼容性**:zigbuild 产物兼容 glibc ≥ 2.17(CentOS 7+ / Ubuntu 16.04+),不受 CI runner 镜像 glibc 版本影响。
- **发布**:tag 触发后自动执行 `napi create-npm-dirs` → `napi artifacts` → `napi pre-publish`,发布 6 个平台子包(`installed-apps-scanner-win32-x64-msvc` 等)+ 主包 `installed-apps-scanner`;使用者 `npm install installed-apps-scanner` 自动选型,无需 Rust 工具链。(npm 上 `installed-apps` 名字已被占用,故包名为 `installed-apps-scanner`;Rust crate 名仍为 `installed-apps`。)
- **发布鉴权(npm Trusted Publishing / OIDC,无需 token)**:
  1. 在 npmjs.com 为**每个包**(主包 + 6 个平台子包)配置 Trusted Publisher:包页 → **Settings → Trusted publishing → GitHub Actions**,填 owner `jardenliu`、repo `installed-apps`、workflow 文件名 `build.yml`(含扩展名);
  2. 配置后 **2 天内完成首次发布**,否则配置过期需重建(发布时才校验绑定);
  3. 若包尚未发布过、没有 Settings 页:先用一次性 token 发首版(Granular Access Token,勾选 **"Bypass two-factor authentication"**,存入 secret `NPM_TOKEN` 并临时在发布步骤传 `NODE_AUTH_TOKEN`),成功后立即为各包配置 Trusted Publishing、删除 token,并可在 Settings → Publishing access 勾选 "Require two-factor authentication and disallow tokens" 加固;
  4. 通过 Trusted Publishing 发布会**自动生成 provenance 构建证明**;`package.json` 的 `repository.url` 必须与 GitHub 仓库一致(已内置,CI 也会强制写入)。
- 发版前在 `bindings/node/package.json` 与 `npm/*/package.json` 对齐版本号(`npx napi version` 可同步)。

### 本地交叉构建(不依赖 CI)

```bash
cd bindings/node
npm run build:local              # 构建本机可行的全部目标
npm run build:local -- x86_64-pc-windows-msvc   # 或指定目标
```

- Windows 本机可产出 win32-x64(原生)与 win32-arm64(需 VS Build Tools ARM64 组件);
- Linux 两个目标需先安装 `pip install ziglang` 与 `cargo install cargo-zigbuild`;
- **darwin 目标无法在 Windows/Linux 上构建**(Apple 工具链限制),请用 macOS 或 CI;
- 产物命名与 CI 完全一致,`node test.mjs` 直接可用。

## 开发

```bash
cargo test        # 单元测试(解析层测试在任意平台可跑)
cargo clippy      # lint
cargo build       # 构建(含示例 CLI: src/bin/ia.rs)
```

网络受限时可使用 crates 镜像,本项目自带 `.cargo/config.toml` 配置了 [rsproxy.cn](https://rsproxy.cn/) 镜像(仅对本项目生效)。

## License

MIT
