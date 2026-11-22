import { useI18n } from "../../lib/i18n";

/**
 * 会话状态点：**颜色之外必须有一份文字**。此前它只是一个按 `data-state` 上色的小圆点
 * （侧栏 / 页签 / 独立窗口标题 / 广播面板 / PopWorkspace 共 6 处），红绿色盲用户与读屏
 * 拿到的是「一个点」——而它表达的恰恰是「能不能往这台机器敲命令」这种最关键的信息。
 *
 * `state` 取 `SessionState`（connected/disconnected/closed）与 UI 侧归一的 idle/connecting，
 * 对应词典 `state.*`；未登记的取值回落到原值，不至于渲染成空标题。
 *
 * `tooltip={false}`：侧栏行 / 页签本身已有「user@host:port」「失败原因」这类整行提示，
 * 圆点再挂一个 title 会把详细提示盖掉——那两处改用 `statusLabel` 把状态并进整行提示。
 */
export function statusLabel(tr: (k: string) => string, state: string): string {
  const key = `state.${state}`;
  const raw = tr(key);
  return raw === key ? state : raw;
}

export default function StatusDot({
  state,
  className = "dot",
  tooltip = true,
}: {
  state: string;
  className?: string;
  tooltip?: boolean;
}) {
  const tr = useI18n();
  const label = statusLabel(tr, state);
  return (
    <span
      className={className}
      data-state={state}
      role="img"
      aria-label={label}
      title={tooltip ? label : undefined}
    />
  );
}
