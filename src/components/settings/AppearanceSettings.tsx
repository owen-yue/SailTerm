import { useEffect } from "react";
import { CUSTOM_THEME_ID, THEME_PRESETS, themeLabel, themePreview } from "../../lib/themes";
import { useI18n } from "../../lib/i18n";
import { appearanceActions, appearanceStore, useStore } from "../../stores";
import { dialog } from "../../lib/tauri";
import Sheet from "../common/Sheet";
import { Row, Sec } from "./settingsUi";

interface Props {
  onClose: () => void;
  /** 嵌入设置中心时不套模态壳。 */
  embedded?: boolean;
}

/**
 * 🎨 外观与主题模态：内置配色预设 + 自定义 JSON 主题导入。
 * 配色（主题）同时驱动终端 xterm 与应用外壳 chrome（`applyAppChrome`）；变更即时热切换运行中的终端
 * （`Terminal.tsx` 只改 xterm options，不重建实例，内容与滚动历史全保留）。
 *
 * 注：字号/字体/行高/字距只作用于终端，已归入设置中心「终端」分类（见 `TerminalFont.tsx`），不在此处。
 *
 * 安全红线：本模态只读写非敏感偏好（颜色），**绝不含任何凭证 / API Key**；
 * 自定义主题经后端 `std::fs` 读取 + 结构校验（缺 background/foreground 即拒），不引入 dialog/fs 插件。
 */
export default function AppearanceSettings({ onClose, embedded = false }: Props) {
  const tr = useI18n();
  const { theme, customTheme, error, discovered, themesDir } = useStore(appearanceStore);

  /** 导入主题 JSON：弹原生打开框选文件（不再手输路径）。 */
  const importTheme = async () => {
    const path = await dialog.pickFile(tr("ap.importTitle"));
    if (!path) return;
    await appearanceActions.importTheme(path);
  };

  useEffect(() => {
    void appearanceActions.refresh();
    void appearanceActions.refreshThemes();
  }, []);

  // 色板卡片：内置预设 + （若已导入）自定义主题
  const cards = [
    ...THEME_PRESETS.map((t) => ({ id: t.id, name: t.name, preview: themePreview(t.id, null) })),
    ...(customTheme
      ? [{ id: CUSTOM_THEME_ID, name: "theme.custom", preview: themePreview(CUSTOM_THEME_ID, customTheme) }]
      : []),
  ];

  return (
    <>
      <Sheet title={tr("ap.title")} onClose={onClose} embedded={embedded}>
          <div className="sec-set-body">
            <Sec title={tr("ap.termColors")}>
              <Row title={tr("ap.theme")} desc={themeLabel(theme, customTheme)} wide>
                <div className="theme-grid">
                  {cards.map((c) => (
                    <button
                      key={c.id}
                      type="button"
                      className="theme-card"
                      data-on={theme === c.id}
                      title={tr(c.name)}
                      onClick={() => void appearanceActions.update({ theme: c.id })}
                    >
                      <span className="tc-swatch" style={{ background: c.preview.bg, color: c.preview.fg }}>
                        <span className="tc-cursor" style={{ background: c.preview.cursor }} />
                        Aa$
                      </span>
                      <span className="tc-name">{tr(c.name)}</span>
                    </button>
                  ))}
                </div>
              </Row>
            </Sec>

            <Sec
              title={tr("ap.customTheme")}
              extra={
                <button type="button" className="link" onClick={() => void importTheme()}>
                  {tr("ap.importBtn")}
                </button>
              }
            >
              <Row
                title={tr("ap.importFromJson")}
                desc={tr("ap.importDesc")}
              />
              {customTheme && (
                <Row
                  title={tr("ap.imported").replace("{n}", customTheme.name)}
                  desc={tr("ap.colorCount").replace("{c}", String(Object.keys(customTheme.colors).length))}
                />
              )}
            </Sec>

            <Sec title={tr("ap.discovered")}>
              {discovered.length === 0 ? (
                <Row title={tr("ap.noDiscovered")} desc={themesDir || tr("ap.discoveredDesc")} />
              ) : (
                <>
                  <Row title={tr("ap.discoveredHint")} desc={themesDir} />
                  <div className="theme-grid">
                    {discovered.map((d) => (
                      <button
                        key={d.path}
                        type="button"
                        className="theme-card"
                        title={d.name}
                        onClick={() => void appearanceActions.importTheme(d.path)}
                      >
                        <span className="tc-name">{d.name}</span>
                      </button>
                    ))}
                  </div>
                </>
              )}
            </Sec>
          </div>

          {error && <div className="form-error">{error}</div>}

          <div className="form-actions sec-set-foot">
            <button type="button" className="ghost" onClick={() => void appearanceActions.reset()}>
              {tr("ap.resetAppearance")}
            </button>
            {!embedded && (
              <button type="button" onClick={onClose}>
                {tr("ap.finish")}
              </button>
            )}
          </div>
      </Sheet>
    </>
  );
}
