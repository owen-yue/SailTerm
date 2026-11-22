import { Component, type ErrorInfo, type ReactNode } from "react";
import { t } from "../../lib/i18n";

interface State {
  error: Error | null;
}

/**
 * 全局渲染错误边界：兜住任意子组件在 render / 生命周期 / effect 里抛出的异常，
 * 降级成一张可恢复的提示卡（重试 / 重新加载），而不是让整棵 React 树卸载 → 整页黑屏。
 *
 * 为什么破例用 class：React 目前只支持 class 组件写错误边界（getDerivedStateFromError /
 * componentDidCatch 无 hook 等价物）。
 *
 * 兜底 UI 刻意「零外部依赖」：只 import React 与 i18n 的命令式 `t`，不引任何 store / 共享组件
 * （按钮直接用全局纯 CSS 类 `.btn`，与 <Button> 同观感但无 JS 依赖）——确保崩溃后这张卡本身
 * 绝不会再因同样的坏依赖二次崩溃。文案在崩溃瞬间渲染一次即可，无需订阅语言变化重渲染。
 */
export default class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // 原始堆栈留控制台，便于排查（与卡面上的错误摘要互补）。
    console.error("[ErrorBoundary] 渲染异常：", error, info.componentStack);
  }

  /** 重试：清空错误态、重新渲染子树。适合偶发/可自愈的异常。 */
  private retry = (): void => this.setState({ error: null });

  /** 重新加载：刷新当前 WebView（Tauri 下即重载本窗口），拉回干净的应用状态。 */
  private reload = (): void => window.location.reload();

  render(): ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="err-boundary">
        <div className="err-boundary-card">
          <div className="err-boundary-title">{t("eb.title")}</div>
          <p className="err-boundary-body">{t("eb.body")}</p>
          <details className="err-boundary-detail">
            <summary>{t("eb.detail")}</summary>
            <pre>{String(error?.message ?? error)}</pre>
          </details>
          <div className="err-boundary-acts">
            <button type="button" className="btn btn--primary" onClick={this.retry}>
              {t("eb.retry")}
            </button>
            <button type="button" className="btn btn--ghost" onClick={this.reload}>
              {t("eb.reload")}
            </button>
          </div>
        </div>
      </div>
    );
  }
}
