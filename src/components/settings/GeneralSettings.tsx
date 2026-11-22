import { availableLocales, useI18n } from "../../lib/i18n";
import { MONITOR_INTERVALS, PANEL_MAX, PANEL_MIN, restartActions, settingsActions, settingsStore, uiPrefs, uiPrefsStore, useStore } from "../../stores";
import { Row, Sec, Toggle } from "./settingsUi";
import Select from "../common/Select";

/**
 * 通用设置：界面语言 + 界面偏好（监控采样间隔、下栏默认高度）。
 *
 * 语言是**注册制**的：内置 zh/en + 插件经 manifest.locales 贡献的语料，启动时注册进 i18n；
 * 这里用 `availableLocales()` 动态渲染选项（含「跟随系统」），新增语言无需改本组件。
 * 语言偏好经后端 `ui.language` 持久化；采样间隔是纯前端偏好（uiPrefs / localStorage）。
 *
 * 数据目录属应用信息，见「关于」页；开发者模式是调试门禁开关，收进「安全」页——通用首屏不再堆这些。
 */
export default function GeneralSettings() {
  const { uiLanguage, osNotifications, error } = useStore(settingsStore);
  const { monitorInterval, panelHeight } = useStore(uiPrefsStore);
  const t = useI18n();
  // 语料注册表变化（插件语料异步到位）时 useI18n 已触发重渲染，这里再取一次最新列表
  const locales = availableLocales();

  return (
    <>
      <Sec title={t("general.language")}>
        <Row title={t("general.language")}>
          <Select
            className="field"
            value={uiLanguage}
            options={[
              { value: "system", label: t("general.lang.system") },
              ...locales.map((l) => ({ value: l.code, label: l.label })),
            ]}
            onChange={(v) => void settingsActions.update({ uiLanguage: v })}
          />
        </Row>
      </Sec>

      <Sec title={t("general.uiPrefs")}>
        <Row title={t("general.monitor")}>
          <Select
            className="field"
            value={String(monitorInterval)}
            options={MONITOR_INTERVALS.map((n) => ({ value: String(n), label: `${n}s` }))}
            onChange={(v) => uiPrefs.set({ monitorInterval: Number(v) })}
          />
        </Row>
        <Row title={t("general.panelHeight")}>
          <span className="font-val">{panelHeight}px</span>
          <input
            type="range"
            min={PANEL_MIN}
            max={PANEL_MAX}
            step={10}
            value={panelHeight}
            onChange={(e) => {
              uiPrefs.set({ panelHeight: Number(e.target.value) });
              restartActions.request(); // 下栏高度重启后生效：点亮常驻重启徽标
            }}
          />
        </Row>
        {/* 命令补全属终端交互行为，见 TerminalPrefs；补全数据源不给用户选（固定仅当前连接，见 Terminal.tsx） */}
      </Sec>

      <Sec title={t("general.notify")}>
        <Row title={t("general.osNotify")}>
          <Toggle
            checked={osNotifications}
            onChange={(on) => void settingsActions.update({ osNotifications: on })}
          />
        </Row>
      </Sec>

      {/* 小文件打包传输：功能暂不稳定，已雪藏——后端默认关（bundle_transfer_enabled 现 unwrap_or(false)），
          用户设置项一并撤下；将来放开时把 Toggle 加回并翻默认值即可。 */}

      {error && <div className="form-error">{error}</div>}
    </>
  );
}
