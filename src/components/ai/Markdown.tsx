import { useMemo } from "react";
import { useI18n } from "../../lib/i18n";
import { copyText } from "../../lib/clipboard";
import { openExternal, sessions as sshApi } from "../../lib/tauri";
import { focusTerm } from "../../lib/termAgent";
import { toast } from "../../stores";
import { parseMarkdown, type Block, type Inline } from "../../lib/markdown";

/** 块内第一条「像是要跑的命令」：跳过空行与 # 注释，取到行尾（写进 PTY 后停在输入行等用户回车）。 */
function firstRunnable(body: string): string | null {
  const line = body
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l && !l.startsWith("#"));
  return line || null;
}

/** 这些围栏语言是「贴回来的内容」而不是命令，不给插入终端（框选提问就把选区包成 ```text）。 */
const NOT_A_COMMAND = new Set(["text", "output", "log", "result"]);

function InlineNodes({ parts, t }: { parts: Inline[]; t: (k: string) => string }) {
  return (
    <>
      {parts.map((p, i) => {
        switch (p.k) {
          case "code":
            return <code key={i}>{p.v}</code>;
          case "bold":
            return (
              <strong key={i}>
                <InlineNodes parts={p.v} t={t} />
              </strong>
            );
          case "em":
            return (
              <em key={i}>
                <InlineNodes parts={p.v} t={t} />
              </em>
            );
          case "link":
            // 不用 <a href>：webview 里点它会试图导航本窗口。仍走系统浏览器，
            // 且按项目惯例用 <button> 而不是给非交互元素挂 onClick（键盘能直接 Enter）。
            return (
              <button
                key={i}
                type="button"
                className="md-link"
                title={p.href}
                onClick={() => {
                  void openExternal(p.href).then((ok) => {
                    if (!ok) toast.show(t("aw.linkFail"), "error");
                  });
                }}
              >
                {p.v}
              </button>
            );
          default:
            return <span key={i}>{p.v}</span>;
        }
      })}
    </>
  );
}

/**
 * 助手回复的 Markdown 渲染（语法子集见 src/lib/markdown.ts）。
 *
 * 代码块沿用既有的 `.ai-code` 卡片（语言栏 + 动作钮 + 等宽横向滚动）——那套样式此前
 * 已无人引用，这次复活；「插入终端」从**整条气泡一个**改成**每个代码块一个**，
 * 一条回复里给多条命令时不再只能插第一条。
 */
export default function Markdown({ text, sessionId }: { text: string; sessionId?: string | null }) {
  const t = useI18n();
  const blocks = useMemo(() => parseMarkdown(text), [text]);
  return (
    <>
      {blocks.map((b, i) => (
        <BlockNode key={i} b={b} sessionId={sessionId} t={t} />
      ))}
    </>
  );
}

function BlockNode({
  b,
  sessionId,
  t,
}: {
  b: Block;
  sessionId?: string | null;
  t: (k: string) => string;
}) {
  switch (b.k) {
    case "code": {
      const cmd = sessionId && !NOT_A_COMMAND.has(b.lang) ? firstRunnable(b.body) : null;
      return (
        <div className="ai-code">
          <div className="ai-code-bar">
            <span className="ai-code-lang">{b.lang || "text"}</span>
            <span className="ai-code-acts">
              <button
                type="button"
                className="ai-code-btn"
                onClick={() => {
                  void copyText(b.body).then((ok) =>
                    toast.show(ok ? t("aw.copied") : t("aw.copyFail"), ok ? "success" : "error")
                  );
                }}
              >
                {t("common.copy")}
              </button>
              {/* 只写命令文本、不带回车：用户可先检查/编辑再自行执行 */}
              {cmd && (
                <button
                  type="button"
                  className="ai-code-btn"
                  title={t("aw.insertCmdTip")}
                  onClick={() => {
                    void sshApi
                      .write(sessionId as string, cmd)
                      .then(() => focusTerm(sessionId ?? null))
                      .catch(() => {});
                  }}
                >
                  {t("aw.insertCmd")}
                </button>
              )}
            </span>
          </div>
          <pre className="ai-code-body">{b.body}</pre>
        </div>
      );
    }
    case "head":
      return <div className={`md-head md-h${Math.min(b.level, 4)}`}>{inline(b.v, t)}</div>;
    case "list":
      // 有序列表用 <ol>：靠 CSS 给 <ul> 编上 decimal 只是看着像，屏幕阅读器念不出序号
      return b.ordered ? (
        <ol className="md-list md-ol">
          {b.items.map((it, i) => (
            <li key={i}>{inline(it, t)}</li>
          ))}
        </ol>
      ) : (
        <ul className="md-list">
          {b.items.map((it, i) => (
            <li key={i}>{inline(it, t)}</li>
          ))}
        </ul>
      );
    case "quote":
      return <div className="md-quote">{inline(b.v, t)}</div>;
    case "hr":
      return <hr className="md-hr" />;
    case "table":
      // 横向滚动容器不能省：右栏宽三百余像素，六列的表不滚动就会把气泡撑破。
      // 表头粘在容器顶（`position: sticky`）让长表滚到中部仍知道各列是什么。
      return (
        <div className="md-table-wrap">
          <table className="md-table">
            <thead>
              <tr>
                {b.head.map((c, i) => (
                  <th key={i} data-align={b.align[i] ?? undefined}>
                    {inline(c, t)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {b.rows.map((r, ri) => (
                <tr key={ri}>
                  {r.map((c, ci) => (
                    <td key={ci} data-align={b.align[ci] ?? undefined}>
                      {inline(c, t)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    default:
      return <div className="md-para">{inline(b.v, t)}</div>;
  }
}

function inline(parts: Inline[], t: (k: string) => string) {
  return <InlineNodes parts={parts} t={t} />;
}
