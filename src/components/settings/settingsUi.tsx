import type { ReactNode } from "react";

/**
 * 设置面板通用原语（参考主流设置页风格：分组小标题 + 灰底卡片，卡片内「左标题/描述、右控件」行）。
 * - `Sec`：一个分组 = 卡片外的小标题（可带右侧动作按钮）+ 卡片容器。
 * - `Row`：卡片内一行；行间分隔线由 CSS（.ss-row + .ss-row）统一加。
 *   `wide` 行用于主题格、长输入等控件需独占一行的场景。
 * - `Toggle`：开关式复选框，全局替代旧方勾（.check 的 input 也由 CSS 统一换成开关外观）。
 */
export function Sec({
  title,
  extra,
  children,
}: {
  title: ReactNode;
  /** 标题行右侧的动作区（如「＋ 新建」）。 */
  extra?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="ss-sec">
      <div className="ss-sec-head">
        {title}
        {extra}
      </div>
      <div className="ss-card">{children}</div>
    </section>
  );
}

export function Row({
  title,
  desc,
  wide,
  children,
}: {
  title: ReactNode;
  /** 标题下的说明文字（灰、小字）。 */
  desc?: ReactNode;
  /** 控件独占一行（网格 / 长输入）。 */
  wide?: boolean;
  /** 右侧控件；缺省则为纯文字行。 */
  children?: ReactNode;
}) {
  return (
    <div className="ss-row" data-wide={wide ? "true" : undefined}>
      <div className="ss-row-txt">
        <div className="ss-row-title">{title}</div>
        {desc ? <div className="ss-row-desc">{desc}</div> : null}
      </div>
      {children ? <div className="ss-row-ctl">{children}</div> : null}
    </div>
  );
}

export function Toggle({
  checked,
  onChange,
  title,
  disabled,
}: {
  checked: boolean;
  onChange: (on: boolean) => void;
  title?: string;
  disabled?: boolean;
}) {
  return (
    <label className="sw" title={title}>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
    </label>
  );
}
