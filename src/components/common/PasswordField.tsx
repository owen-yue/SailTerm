import { forwardRef, useState } from "react";
import { useI18n } from "../../lib/i18n";
import { IconEye, IconEyeOff } from "./icons";

interface Props {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  /** 自动填充：备份主密码用 "new-password"，普通凭证用 "off"。 */
  autoComplete?: string;
  spellCheck?: boolean;
  /** 该框存在已保存的凭证（编辑老连接时）：框空时在右侧挂一枚「已保存」小徽章，
   * 配合调用方传入的灰点占位符传达“已存、输入即覆盖”——真实值绝不回填。 */
  saved?: boolean;
  /** 「已保存」徽章文案（已翻译）。仅在 saved 且框为空时展示。 */
  savedLabel?: string;
}

/**
 * 密码输入框 + 右侧常驻「显示/隐藏」眼睛。
 *
 * 为什么自绘：WebView2（Edge 内核）原生那只 `::-ms-reveal` 眼睛只在「聚焦 + 有内容」时浮现、
 * 失焦即消失（浏览器 chrome，页面无法让它常驻）。这里自绘一只：有内容就一直显示、点击切明暗文，
 * 观感稳定且跟随主题。原生那只由 App.css 的 `input::-ms-reveal{display:none}` 全局关掉，避免重复。
 */
export const PasswordField = forwardRef<HTMLInputElement, Props>(function PasswordField(
  { value, onChange, placeholder, autoComplete = "off", spellCheck = false, saved, savedLabel },
  ref,
) {
  const t = useI18n();
  const [show, setShow] = useState(false);
  const label = show ? t("common.hidePassword") : t("common.showPassword");
  // 徽章与眼睛互斥：有内容→眼睛（切明暗）；空且已保存→「已保存」徽章。
  const chip = !!saved && value.length === 0 && !!savedLabel;
  return (
    <div className={chip ? "pw-field pw-has-chip" : "pw-field"}>
      <input
        ref={ref}
        type={show ? "text" : "password"}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        autoComplete={autoComplete}
        spellCheck={spellCheck}
      />
      {chip && (
        <span className="pw-chip" aria-hidden>
          {savedLabel}
        </span>
      )}
      {value.length > 0 && (
        <button
          type="button"
          className="pw-eye"
          // 眼睛不参与 Tab 顺序：纯辅助控件，键盘用户本可直接看已输入内容。
          tabIndex={-1}
          aria-label={label}
          title={label}
          // 阻止 mousedown 默认行为：既不让按钮抢走输入框焦点，也避免点击冒泡触发外层 <label> 聚焦。
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => setShow((s) => !s)}
        >
          {show ? <IconEyeOff /> : <IconEye />}
        </button>
      )}
    </div>
  );
});

export default PasswordField;
