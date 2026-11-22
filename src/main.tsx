import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./components/common/ErrorBoundary";
import LockOverlay from "./components/common/LockOverlay";
import PinSetupDialog from "./components/common/PinSetupDialog";
import PopWorkspace from "./components/layout/PopWorkspace";
import ToolWindow from "./tools/ToolWindow";
import { installLockClient } from "./lib/lockClient";
import { setPopSession } from "./lib/windows";
import { installModalDrag } from "./lib/modalDrag";
import { isMacOS } from "./lib/platform";
import { applyAppChrome, readChromeCache } from "./lib/themes";

// 弹出窗口路由：主窗口用 WebviewWindow 创建独立会话窗口时带 `?pop=<sessionId>`，
// 命中则渲染极简工作区（ownsSession 判定的依据也在这里写入）；否则渲染完整主界面。
// 工具窗口路由：工具独立窗口带 `?tool=<id>`，命中则渲染该工具的独立工作区 ToolWindow。
const params = new URLSearchParams(window.location.search);
const pop = params.get("pop");
const tool = params.get("tool");

// 平台标记：macOS 用系统原生红绿灯（Overlay 标题栏），顶栏需给左上角按钮留位；
// 其余平台沿用自绘窗口控件。写到 <html data-os> 供全局 CSS 选择器消费。
// 谓词与 lib/platform.isMacOS()（窗口装饰分流）保持同源，避免留位与红绿灯错配。
document.documentElement.dataset.os = isMacOS() ? "mac" : "win";

// 同步预着色：任何窗口（尤其新开的工具/会话弹出窗）首帧即用上次已知主题，
// 消除「等后端 refresh 期间白/错色」闪烁；refresh() 随后仍会拉取权威值校正。
const cachedChrome = readChromeCache();
if (cachedChrome) applyAppChrome(cachedChrome.id, cachedChrome.custom ?? null);

// 弹框拖拽：在入口顶层安装一次，而非某个根组件的 effect——本前端按 URL 分三个根
// （主窗口 App / 会话弹出窗 PopWorkspace / 工具窗 ToolWindow），每个 WebView 有独立
// document。放在这里才能三种窗口全覆盖（否则弹出窗里的弹框拖不动）。
installModalDrag();

// 锁屏客户端胶水：订阅锁态广播 + 空闲心跳 + 锁定输入闸门。与 installModalDrag 同理
// 装在入口顶层：三个根（主窗/会话弹出窗/工具窗）各自独立 document，都要覆盖；
// 且在 ReactDOM 渲染前注册，window capture 监听才能抢在 App 各快捷键 effect 之前。
installLockClient();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {/* 全局错误边界：兜住三个根（主窗 / 会话弹出窗 / 工具窗）任意子组件的渲染异常，
        降级成可恢复提示卡而非整页黑屏。包在最外层，一个 WebView 一份。 */}
    <ErrorBoundary>
      <>
        {pop ? (
          <PopRoot sessionId={pop} />
        ) : tool ? (
          <ToolWindow id={tool} />
        ) : (
          <App />
        )}
        {/* 锁屏遮罩：三根共用的唯一挂载点，locked 时盖住一切（含顶栏窗口控件）；
            底下组件保持挂载不卸载——解锁即原位恢复、零丢失。 */}
        <LockOverlay />
        {/* 无 PIN 时的「设置 PIN」弹窗：同为三根挂载（弹出窗/工具窗的锁屏入口也要能就地建码）。 */}
        <PinSetupDialog />
      </>
    </ErrorBoundary>
  </React.StrictMode>,
);

/** 先登记 popSid（供 ownsSession 过滤），再挂工作区。 */
function PopRoot({ sessionId }: { sessionId: string }) {
  setPopSession(sessionId);
  return <PopWorkspace sessionId={sessionId} />;
}
