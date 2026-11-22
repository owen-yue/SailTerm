import { useEffect, useState } from "react";
import { sessions as sessionsApi } from "../../lib/tauri";
import { terminalActions, terminalStore, uiPrefs, uiPrefsStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";
import Select from "../common/Select";
import { Row, Sec, Toggle } from "./settingsUi";

/**
 * 终端分类里本模块自有的一节：操作行为 / 滚动缓冲 / 本地 Shell。
 * （字号字体见 terminal/TerminalFont，片段与触发器是既有管理器的嵌入态。）
 */
export default function TerminalPrefs() {
  const { scrollback, cursorBlink, localShell, copyOnSelect, cursorStyle, bellStyle, keywordHighlight, error } =
    useStore(terminalStore);
  const { completionEnabled } = useStore(uiPrefsStore);
  const t = useI18n();
  // WSL 探测：引擎装着就显示选择器（哪怕没有可交互发行版）；distros 供具体选项（已剔除 docker-desktop 等）。
  const [wsl, setWsl] = useState<{ installed: boolean; distros: string[] }>({
    installed: false,
    distros: [],
  });
  useEffect(() => {
    void sessionsApi
      .wslList()
      .then(setWsl)
      .catch(() => setWsl({ installed: false, distros: [] }));
  }, []);
  return (
    <>
      <Sec title={t("term.behavior")}>
        <Row title={t("term.copyOnSelect")}>
          <Toggle
            checked={copyOnSelect}
            onChange={(on) => void terminalActions.update({ copyOnSelect: on })}
          />
        </Row>
        <Row title={t("term.keywordHighlight")}>
          <Toggle
            checked={keywordHighlight}
            onChange={(on) => void terminalActions.update({ keywordHighlight: on })}
          />
        </Row>
        <Row title={t("term.cursor.style")}>
          <Select
            className="field"
            value={cursorStyle}
            options={(["block", "bar", "underline"] as const).map((s) => ({ value: s, label: t(`term.cursor.${s}`) }))}
            onChange={(v) => void terminalActions.update({ cursorStyle: v as typeof cursorStyle })}
          />
        </Row>
        <Row title={t("term.bell")}>
          <Select
            className="field"
            value={bellStyle}
            options={(["none", "visual", "sound"] as const).map((s) => ({ value: s, label: t(`term.bell.${s}`) }))}
            onChange={(v) => void terminalActions.update({ bellStyle: v as typeof bellStyle })}
          />
        </Row>
        <Row title={t("sc.cursorBlink")}>
          <Toggle
            checked={cursorBlink}
            onChange={(on) => void terminalActions.update({ cursorBlink: on })}
          />
        </Row>
        <Row title={t("term.completionEnabled")}>
          <Toggle
            checked={completionEnabled}
            onChange={(on) => uiPrefs.set({ completionEnabled: on })}
            title={t("term.completionEnabled")}
          />
        </Row>
        {/* 补全数据源不给用户选：固定仅当前连接（见 Terminal.tsx）；uiPrefs.completionHistoryScope 字段保留 */}
      </Sec>
      <Sec title={t("sc.scrollbackSec")}>
        <Row title={t("sc.lines")}>
          <input
            type="number"
            min={100}
            max={100000}
            value={scrollback}
            onChange={(e) => void terminalActions.update({ scrollback: Number(e.target.value) || 0 })}
          />
        </Row>
      </Sec>
      <Sec title={t("sc.localShellSec")}>
        {wsl.installed && (
          <Row
            title={t("sc.wslPick")}
            wide
          >
            <Select
              className="field"
              value={localShell.startsWith("wsl:") ? localShell : ""}
              options={[
                { value: "", label: t("sc.shellDefault") },
                { value: "wsl:", label: t("sc.wslDefault") },
                ...wsl.distros.map((d) => ({ value: `wsl:${d}`, label: `WSL · ${d}` })),
              ]}
              onChange={(v) => void terminalActions.update({ localShell: v })}
            />
          </Row>
        )}
        <Row title={t("sc.path")} wide>
          <input
            value={localShell}
            spellCheck={false}
            placeholder={t("sc.pathDesc")}
            onChange={(e) => void terminalActions.update({ localShell: e.target.value })}
            onBlur={() => void terminalActions.update({ localShell: localShell.trim() })}
          />
        </Row>
      </Sec>
      {error && <div className="form-error">{error}</div>}
      <div className="form-actions sec-set-foot">
        <button type="button" className="ghost" onClick={() => void terminalActions.reset()}>
          {t("sc.reset")}
        </button>
      </div>
    </>
  );
}
