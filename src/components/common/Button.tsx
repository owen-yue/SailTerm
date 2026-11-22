import type { ButtonHTMLAttributes, ReactNode } from "react";

/**
 * 全站统一按钮。自包含样式（`.btn` + 变体类），不依赖任何祖先容器，
 * 避免按钮散落到不同容器时退回浏览器默认灰框。
 *
 * 变体（variant）：
 * - `primary` 主操作：品牌渐变底（默认）
 * - `ghost`   次操作：灰底、弱化文字
 * - `outline` 描边次操作：透明底 + 边线，悬停描品牌色
 * - `link`    文字按钮：无边框无底色
 * `danger` 叠加态：把文字染成危险红（常配 `link`/`outline`）。`size="sm"` 收紧内边距。
 * `loading`：异步进行中——自动禁用并在水印位置（文字左侧）渲染转圈动画，
 * 供「保存 / 测试 / 连接」这类需等待远端回执的按钮统一复用（CSS 见 `.btn-spin`）。
 */
type Variant = "primary" | "ghost" | "outline" | "link";

interface Props extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "className"> {
  variant?: Variant;
  size?: "sm" | "md";
  danger?: boolean;
  /** 异步进行中：禁用按钮并在文字左侧渲染转圈。 */
  loading?: boolean;
  className?: string;
  children: ReactNode;
}

export default function Button({
  variant = "primary",
  size = "md",
  danger,
  loading = false,
  className,
  children,
  type = "button",
  disabled,
  ...rest
}: Props) {
  const cls = [
    "btn",
    `btn--${variant}`,
    danger ? "btn--danger" : "",
    size === "sm" ? "btn--sm" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button type={type} className={cls} disabled={loading || disabled} {...rest}>
      {loading && <span className="btn-spin" aria-hidden="true" />}
      {children}
    </button>
  );
}
