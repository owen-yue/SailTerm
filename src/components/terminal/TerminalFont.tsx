import { useEffect, useState } from "react";
import { useI18n } from "../../lib/i18n";
import { DEFAULT_FONT_FAMILY, DEFAULT_FONT_SIZE } from "../../lib/themes";
import { appearanceActions, appearanceStore, uiPrefs, uiPrefsStore, useStore } from "../../stores";
import { Row, Sec } from "../settings/settingsUi";

/** 常见等宽字体族快捷选项（也可在输入框自由填写任意 CSS font-family）。 */
const FONT_PRESETS = [
  "'Cascadia Code', 'JetBrains Mono', Consolas, monospace",
  "'JetBrains Mono', Menlo, monospace",
  "Menlo, Monaco, 'Courier New', monospace",
  "Consolas, 'Courier New', monospace",
  "'Fira Code', monospace",
];

/** 字号可调区间（与后端 `clamp_font_size` 一致）。 */
const FONT_MIN = 8;
const FONT_MAX = 32;

/**
 * ⌨️ 终端字体与排版（自「外观」迁入设置中心「终端」分类）：字号/字体走后端 appearance 偏好，
 * 行高/字距走本机 uiPrefs（localStorage）。四项全部只作用于终端 xterm（见 `Terminal.tsx`
 * `term.options.*`），不影响应用外壳 UI，故归入终端而非外观。变更即时热切换运行中的终端，不重建实例。
 *
 * 安全红线：只读写非敏感偏好（字体/排版），**绝不含任何凭证 / API Key**。
 */
export default function TerminalFont() {
  const tr = useI18n();
  const { fontSize, fontFamily } = useStore(appearanceStore);
  const { lineHeight, letterSpacing } = useStore(uiPrefsStore);
  const [fontDraft, setFontDraft] = useState(fontFamily);

  // 后端 refresh/其他入口改了字体时，同步输入框草稿
  useEffect(() => setFontDraft(fontFamily), [fontFamily]);

  const stepFont = (delta: number) => {
    const next = Math.min(FONT_MAX, Math.max(FONT_MIN, Math.round(fontSize + delta)));
    if (next !== fontSize) void appearanceActions.update({ fontSize: next });
  };
  const commitFont = () => {
    const v = fontDraft.trim();
    if (v && v !== fontFamily) void appearanceActions.update({ fontFamily: v });
    else setFontDraft(fontFamily);
  };
  // 本段自带复位：字号/字体回后端默认，行高/字距回本机 uiPrefs 默认（与「外观/终端」的全局 reset 解耦）。
  const resetFont = () => {
    void appearanceActions.update({ fontSize: DEFAULT_FONT_SIZE, fontFamily: DEFAULT_FONT_FAMILY });
    setFontDraft(DEFAULT_FONT_FAMILY);
    uiPrefs.set({ lineHeight: 1, letterSpacing: 0 });
  };

  return (
    <Sec
      title={tr("ap.fontTypography")}
      extra={
        <button type="button" className="link" onClick={resetFont}>
          {tr("ap.resetFont")}
        </button>
      }
    >
      <Row title={tr("ap.fontSize")} desc={`${FONT_MIN}–${FONT_MAX}px`}>
        <button type="button" className="icon-btn" title={tr("ap.fontDecrease")} onClick={() => stepFont(-1)}>
          −
        </button>
        <span className="font-val">{fontSize}px</span>
        <button type="button" className="icon-btn" title={tr("ap.fontIncrease")} onClick={() => stepFont(1)}>
          ＋
        </button>
      </Row>
      <Row title={tr("ap.font")} desc={tr("ap.fontDesc")} wide>
        <div className="font-input">
          <input
            value={fontDraft}
            spellCheck={false}
            onChange={(e) => setFontDraft(e.target.value)}
            onBlur={commitFont}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitFont();
            }}
          />
          <button type="button" onClick={commitFont}>
            {tr("common.apply")}
          </button>
        </div>
        <div className="seg font-presets">
          {FONT_PRESETS.map((f) => (
            <button
              key={f}
              type="button"
              data-on={fontFamily === f}
              title={f}
              onClick={() => void appearanceActions.update({ fontFamily: f })}
            >
              {f.split(",")[0].replace(/['"]/g, "")}
            </button>
          ))}
        </div>
      </Row>
      <Row title={tr("ap.lineHeight")} desc={tr("ap.lineHeightDesc")}>
        <span className="font-val">{lineHeight.toFixed(2)}×</span>
        <input
          type="range"
          min={1}
          max={2}
          step={0.05}
          value={lineHeight}
          onChange={(e) => uiPrefs.set({ lineHeight: Number(e.target.value) })}
        />
      </Row>
      <Row title={tr("ap.letterSpacing")}>
        <span className="font-val">{letterSpacing.toFixed(1)}px</span>
        <input
          type="range"
          min={0}
          max={4}
          step={0.5}
          value={letterSpacing}
          onChange={(e) => uiPrefs.set({ letterSpacing: Number(e.target.value) })}
        />
      </Row>
    </Sec>
  );
}
