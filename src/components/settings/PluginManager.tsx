import { useEffect, useState } from "react";
import { IconPuzzle } from "../common/icons";
import { plugins as api, type InstalledPlugin, type PluginSettingsView } from "../../lib/tauri/plugin";
import { errMsg } from "../../lib/tauri/client";
import Popconfirm from "../common/Popconfirm";
import Sheet from "../common/Sheet";
import Select from "../common/Select";
import { useI18n } from "../../lib/i18n";

interface Props {
  onClose: () => void;
  embedded?: boolean;
}

// 与后端 `Permission::as_str()` 的八个值逐一对齐：漏一个就会把原始码（session_write）直出界面。
const PERM_LABEL: Record<string, string> = {
  storage: "pl.permStorage",
  session_exec: "pl.permSessionExec",
  session_write: "pl.permSessionWrite",
  notify: "pl.permNotify",
  log: "pl.permLog",
  network: "pl.permNetwork",
  bridge: "pl.permBridge",
  ai_tool: "pl.permAiTool",
};

/**
 * 🧩 插件管理：列表 / 权限 / 动态配置（manifest.settings）/ 安装本地目录 / 卸载。
 * 命令包与 wasm 逻辑插件共用此面板；配置按插件 id 隔离。
 */
export default function PluginManager({ onClose, embedded = false }: Props) {
  const t = useI18n();
  const [items, setItems] = useState<InstalledPlugin[]>([]);
  const [uninstalling, setUninstalling] = useState<{ id: string; name: string; x: number; y: number } | null>(null);
  const [selected, setSelected] = useState<InstalledPlugin | null>(null);
  const [view, setView] = useState<PluginSettingsView | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const refresh = async () => {
    try {
      const list = await api.list();
      setItems(list);
      setError("");
    } catch (e) {
      setError(errMsg(e));
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const open = async (p: InstalledPlugin) => {
    setSelected(p);
    try {
      setView(await api.settings(p.manifest.id));
    } catch (e) {
      setError(errMsg(e));
    }
  };

  const saveSetting = async (key: string, value: string) => {
    if (!selected) return;
    await api.setSetting(selected.manifest.id, key, value);
    setView((v) => (v ? { ...v, values: { ...v.values, [key]: value } } : v));
    setNotice(t("pl.saved"));
    setTimeout(() => setNotice(""), 1200);
  };

  const install = async (path: string) => {
    const p = await api.install(path);
    if (!p) throw new Error(t("pl.installFail"));
    await refresh();
    await open(p);
  };

  /** 弹原生目录选择框挑插件目录，选完直接安装（取消则什么都不做）。 */
  const installDir = async () => {
    try {
      const dir = await api.pickDir(t("dlg.pluginDir"));
      if (!dir) return;
      await install(dir);
    } catch (e) {
      setError(errMsg(e));
    }
  };

  return (
    <>
    <Sheet title={<><IconPuzzle /> {t("pl.title")}</>} onClose={onClose} embedded={embedded}>
      <div className="sec-set-body">
        <section className="ss-sec">
          <div className="ss-sec-head">
            {t("pl.installed")}
            <span className="ss-count">{items.length}</span>
            <button type="button" className="link" onClick={() => void refresh()}>
              {t("common.refresh")}
            </button>
            <button type="button" className="link" onClick={() => void installDir()}>
              ＋ {t("pl.installDir")}
            </button>
          </div>
          {items.length === 0 && (
            <div className="ss-empty">
              {t("pl.emptyBefore")}<code>manifest.json</code>{t("pl.emptyMid")}<code>plugin.wasm</code>{t("pl.emptyAfter")}
            </div>
          )}
          {items.map((p) => (
            <div key={p.manifest.id} className="trg-item">
              <span className="trg-name">{p.manifest.name}</span>
              <span className="trg-act">v{p.manifest.version}</span>
              <span className="trg-act">{p.hasWasm ? "wasm" : t("pl.cmdPack")}</span>
              <span className="trg-pat">
                {p.manifest.permissions.map((x) => t(PERM_LABEL[x] ?? x)).join(" · ") || "—"}
              </span>
              <button type="button" className="link" onClick={() => void open(p)}>
                {t("pl.configure")}
              </button>
              <button
                type="button"
                className="link danger"
                onClick={(e) => {
                  const r = e.currentTarget.getBoundingClientRect();
                  setUninstalling({ id: p.manifest.id, name: p.manifest.name, x: r.left, y: r.bottom + 6 });
                }}
              >
                {t("pl.uninstall")}
              </button>
            </div>
          ))}
        </section>

        {selected && view && (
          <section className="ss-sec">
            <div className="ss-sec-head">
              {t("pl.config")} · {selected.manifest.name}
              {notice && <span className="ss-count">{notice}</span>}
            </div>
            {view.specs.length === 0 && <div className="ss-empty">{t("pl.noSettings")}</div>}
            {view.specs.length > 0 && (
              <div className="ss-card ss-card-form">
              {view.specs.map((s) => (
                <label key={s.key}>
                  {s.label || s.key}
                  {s.type === "boolean" ? (
                    <span className="check">
                      <input
                        type="checkbox"
                        checked={view.values[s.key] === "true" || view.values[s.key] === "1"}
                        onChange={(e) => void saveSetting(s.key, e.target.checked ? "true" : "false")}
                      />
                    </span>
                  ) : s.type === "select" ? (
                    <Select
                      className="field"
                      value={view.values[s.key] ?? s.default}
                      options={(s.options?.length ? s.options : [s.default]).map((o) => ({
                        value: o,
                        label: o,
                      }))}
                      onChange={(v) => void saveSetting(s.key, v)}
                    />
                  ) : (
                    <input
                      type={s.type === "number" ? "number" : "text"}
                      value={view.values[s.key] ?? s.default}
                      onChange={(e) => void saveSetting(s.key, e.target.value)}
                    />
                  )}
                </label>
              ))}
              </div>
            )}
            {selected.manifest.commands.length > 0 && (
              <>
                <div className="ss-sec-head" style={{ marginTop: 14 }}>
                  {t("pl.cmdPack")}
                </div>
                {selected.manifest.commands.map((c) => (
                  <div key={c.id} className="trg-item">
                    <span className="trg-name">{c.title}</span>
                    <code className="trg-pat">{c.template}</code>
                  </div>
                ))}
              </>
            )}
          </section>
        )}
      </div>

      {error && <div className="form-error">{error}</div>}

      <div className="form-actions sec-set-foot">
        {!embedded && (
          <button type="button" onClick={onClose}>
            {t("pl.finish")}
          </button>
        )}
      </div>
    </Sheet>
      {uninstalling && (
        <Popconfirm
          x={uninstalling.x}
          y={uninstalling.y}
          title={t("pl.uninstallMsg").replace("{n}", uninstalling.name)}
          okText={t("pl.uninstall")}
          onConfirm={() => {
            const id = uninstalling.id;
            setUninstalling(null);
            void (async () => {
              await api.uninstall(id);
              setSelected(null);
              setView(null);
              await refresh();
            })();
          }}
          onClose={() => setUninstalling(null)}
        />
      )}
    </>
  );
}
