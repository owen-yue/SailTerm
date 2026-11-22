# 发布

## Windows / NSIS

```powershell
# 在 SailTerm 目录
powershell -ExecutionPolicy Bypass -File scripts\package.ps1
```

产物拷到 `dist-bundle\`。

## macOS / app + dmg

**必须在 Mac 上执行**（要 `codesign` / `hdiutil`；Windows 无法出 macOS 包）：

```bash
# 依赖：Xcode Command Line Tools、Rust、pnpm
xcode-select --install
rustup target add aarch64-apple-darwin   # Apple Silicon 本机
# rustup target add x86_64-apple-darwin  # 如需 Intel 交叉/通用

cd SailTerm
bash scripts/package-macos.sh
# 或手动：
pnpm tauri build --bundles app,dmg
```

产物：
- `src-tauri/target/release/bundle/macos/sailterm.app`
- `src-tauri/target/release/bundle/dmg/sailterm_0.1.0_aarch64.dmg`（或 x64）

脚本会拷到 `dist-bundle/`。

### 签名 / 公证（上架或对外分发）

```bash
# 1) 开发者 ID 证书装在「钥匙串」后，在 tauri.conf.json 或环境变量指定：
#    bundle.macOS.signingIdentity = "Developer ID Application: …"
# 2) 公证（需 App Store Connect API 密钥）：
xcrun notarytool submit dist-bundle/sailterm_*.dmg \
  --apple-id you@example.com --team-id TEAMID --password app-specific-pw \
  --wait
xcrun stapler staple dist-bundle/sailterm_*.dmg
```

未签名的本地构建可本机运行；发给他人会被 Gatekeeper 拦（右键打开，或 `xattr -cr sailterm.app`）。

## Linux / deb + AppImage

**必须在 Linux 上执行**（deb/AppImage 无法在 Windows/macOS 交叉产出）。首次上 Linux 机预期要修若干编译问题——Linux 代码路径（`LinuxPortLister`、`xdg-open` 分支等）从未被真机编译验证过，属已知风险（见「平台适配」设计讨论）。

```bash
# 依赖（Ubuntu 22.04+ / Debian 12+）：
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
     libjavascriptcoregtk-4.1-dev librsvg2-dev patchelf file
# 另需 Node 20+ / pnpm / rustup stable（与 CONTRIBUTING.md 一致）

cd SailTerm
bash scripts/package-linux.sh
# 或手动：
pnpm tauri build --bundles deb,appimage
```

产物（脚本拷到 `dist-bundle/` 并打印 sha256）：
- `src-tauri/target/release/bundle/deb/SailTerm_<ver>_amd64.deb`
- `src-tauri/target/release/bundle/appimage/SailTerm_<ver>_amd64.AppImage`

注意事项：
- AppImage 打包会联网从 GitHub 下载 linuxdeploy 插件与 appimagetool（缓存在 `~/.cache/tauri`）；不通时先设 `https_proxy`。
- deb 运行时依赖已在 `tauri.conf.json` 的 `bundle.linux.deb.depends` 显式声明（webkit2gtk 4.1 / gtk3）。
- AppImage 终端用户机需 `libfuse2`（Ubuntu 24.04 起默认只有 fuse3，需装 libfuse2 或 `--appimage-extract-and-run`）。
- aarch64 机器上直接构建即出 `arm64` 包，无需交叉配置。

## 手工步骤（通用）

1. `pnpm check` — 类型 + 前端构建
2. Windows：`pnpm tauri build --bundles nsis`；macOS：`pnpm tauri build --bundles app,dmg`；Linux：`pnpm tauri build --bundles deb,appimage`
3. 签名/公证：macOS 见上；Windows 可选签名；Linux 用 AppImage/deb
4. 自动更新：GitHub Releases + minisign（证书就绪后接 tauri-plugin-updater）

## 本机已知坑（Windows + 360）

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `could not execute process ...\build-script-build (拒绝访问)` | 360 拦截 release 构建脚本 | 360 信任 `E:\myproject\ai-ssh` 与 cargo target 目录；或**多跑几次** `cargo build --release`（会逐个放行） |
| `Failed to open file: PermissionDenied` / `timeout: global`（bundle 阶段） | 360 拦截 NSIS 解压 | 已预装到 `%LOCALAPPDATA%\tauri\NSIS`（含 `nsis_tauri_utils.dll`）；勿删该目录 |
| 打包运行弹 `cmd.exe 0xc0000142` | 启动 ConPTY 预热曾 spawn cmd | 已改为只预热 PTY；见 `local.rs warmup_conpty` |
| GitHub 下载超时 | 网络 | NSIS 工具链已缓存；插件 DLL SHA1=`75197FEE3C6A814FE035788D1C34EAD39349B860` |

NSIS 目录完整性：`makensis.exe`、`Bin/makensis.exe`、`Stubs/lzma-x86-unicode`、
`Plugins/x86-unicode/additional/nsis_tauri_utils.dll` 等见 Tauri `NSIS_REQUIRED_FILES`。

## 输出

- Windows 安装包：`<target>/release/bundle/nsis/sailterm_<ver>_x64-setup.exe`
- macOS：`…/bundle/macos/sailterm.app`、`…/bundle/dmg/sailterm_<ver>_<arch>.dmg`
- Linux：`…/bundle/deb/SailTerm_<ver>_<arch>.deb`、`…/bundle/appimage/SailTerm_<ver>_<arch>.AppImage`
- 绿色 exe / app 内二进制：`<target>/release/sailterm(.exe)`
