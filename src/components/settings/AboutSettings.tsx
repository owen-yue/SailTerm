import { useRef, useState } from "react";
import logo from "../../assets/sailterm-logo.png";
import {
  app as appApi,
  errMsg,
  updater,
  type MoveProgress,
  type Update,
} from "../../lib/tauri";
import { toast } from "../../stores";
import type { AppInfo } from "../../types";
import { useI18n } from "../../lib/i18n";
import { FEATURES } from "../../lib/features";
import { Row, Sec } from "./settingsUi";
import Button from "../common/Button";
import DirPicker from "../common/DirPicker";
import ConfirmDialog from "../dialog/ConfirmDialog";
import MigrationProgress from "../terminal/MigrationProgress";

/** 迁移预检拒绝原因码 → i18n 键（后端 data_dir_check 返回 same/nested/hasData/empty）。 */
const MIGRATE_ERR_KEY: Record<string, string> = {
  same: "about.migrateErrSame",
  nested: "about.migrateErrNested",
  hasData: "about.migrateErrHasData",
  empty: "about.migrateErrGeneric",
};

/** 运行平台标签：从 UA 粗判操作系统名（未引入 os 插件，够用即可）。 */
function platformLabel(): string {
  const ua = typeof navigator !== "undefined" ? navigator.userAgent : "";
  if (/Windows NT 10/.test(ua)) return "Windows";
  if (/Windows/.test(ua)) return "Windows";
  if (/Macintosh|Mac OS X/.test(ua)) return "macOS";
  if (/Android/.test(ua)) return "Android";
  if (/(X11|Linux)/.test(ua)) return "Linux";
  return "—";
}

/** 关于：应用信息 + 自动更新 + 数据目录（打开/迁移）。`info` 由设置中心统一取一次。 */
export default function AboutSettings({ info }: { info: AppInfo | null }) {
  const t = useI18n();
  // 自动更新：主动检查 → 下载并安装 → 复用后端 restart_app 重启生效。
  // 端点/公钥未配置（占位）或离线时 check() 抛错，落到 error 态轻提示，不影响其余功能。
  const updateRef = useRef<Update | null>(null);
  const [upd, setUpd] = useState<
    | { phase: "idle" }
    | { phase: "checking" }
    | { phase: "upToDate" }
    | { phase: "available"; version: string; body: string | null }
    | { phase: "installing"; percent: number | null }
    | { phase: "installed" }
    | { phase: "error"; msg: string }
  >({ phase: "idle" });
  const checkUpdate = async () => {
    setUpd({ phase: "checking" });
    try {
      const res = await updater.check();
      if (res) {
        updateRef.current = res.update;
        setUpd({ phase: "available", version: res.info.version, body: res.info.body });
      } else {
        setUpd({ phase: "upToDate" });
      }
    } catch (e) {
      setUpd({ phase: "error", msg: errMsg(e) });
    }
  };
  const installUpdate = async () => {
    const u = updateRef.current;
    if (!u) return;
    setUpd({ phase: "installing", percent: 0 });
    try {
      await updater.install(u, (p) => setUpd({ phase: "installing", percent: p }));
      setUpd({ phase: "installed" });
    } catch (e) {
      setUpd({ phase: "error", msg: errMsg(e) });
    }
  };
  const openDataDir = async () => {
    if (!info?.dataDir) return;
    try {
      // 走后端直接开目录：数据目录可能被迁移到任意路径，前端 openPath 受 $APPDATA scope 限制会 ForbiddenPath。
      await appApi.openDataDir();
    } catch {
      // 后端开目录失败时退化为复制路径
      try {
        await navigator.clipboard.writeText(info.dataDir);
      } catch {
        /* 静默 */
      }
    }
  };
  // 迁移向导：选目标目录 → 二次确认 → 进度弹框（实时）→ 完成后给「立即重启」。
  const [migrateTo, setMigrateTo] = useState<string | null>(null);
  const [move, setMove] = useState<{
    target: string;
    progress: MoveProgress | null;
    done: boolean;
    error: string | null;
  } | null>(null);
  const handleDirPicked = async (p: string) => {
    if (move) return;
    // 先做轻量预检：不合格直接 toast 轻提示，不弹重量级进度框。
    const reason = await appApi.dataDirCheck(p);
    if (reason) {
      const key = MIGRATE_ERR_KEY[reason];
      toast.show(key ? t(key) : t("about.migrateErrGeneric"), "warn");
      return;
    }
    setMigrateTo(p);
  };
  const confirmMigrate = async () => {
    if (!migrateTo) return;
    const to = migrateTo;
    setMigrateTo(null);
    setMove({
      target: to,
      progress: { phase: "files", done: 0, total: 0, percent: 0, current: "" },
      done: false,
      error: null,
    });
    const un = await appApi.onMoveProgress((p) =>
      setMove((m) => (m && !m.done && !m.error ? { ...m, progress: p } : m)),
    );
    try {
      await appApi.dataDirMove(to);
      setMove((m) =>
        m
          ? {
              ...m,
              done: true,
              progress: m.progress ? { ...m.progress, phase: "done", percent: 100 } : m.progress,
            }
          : m,
      );
    } catch (e) {
      setMove((m) => (m ? { ...m, error: errMsg(e) } : m));
    } finally {
      un();
    }
  };
  const vault =
    info?.vaultBackend === "os-keyring"
      ? t("about.vaultKeyring")
      : info?.vaultBackend === "aes-gcm-file"
        ? t("about.vaultFile")
        : (info?.vaultBackend ?? "…");
  return (
    <>
      {/* 品牌区：复用应用图标本体，与 Dock / 首页图标完全一致 */}
      <div className="about-hero">
        <img className="about-logo" src={logo} alt="SailTerm" />
        <div className="about-name">SailTerm</div>
        {info && <div className="about-ver">v{info.version}</div>}
        <p className="about-tag">{t("about.tagline")}</p>
      </div>

      <Sec title={t("about.infoSec")}>
        <Row title={t("about.version")}>
          <span className="about-val">{info ? info.version : "…"}</span>
        </Row>
        <Row title={t("about.tech")}>
          <span className="about-val">Tauri v2 · React · Rust</span>
        </Row>
        <Row title={t("about.platform")}>
          <span className="about-val">{platformLabel()}</span>
        </Row>
        <Row title={t("about.vault")}>
          <span className="about-val">{vault}</span>
        </Row>
        <Row title={t("about.dataDir")} wide>
          <DirPicker
            value={info?.dataDir ?? ""}
            onChange={(p) => void handleDirPicked(p)}
            readOnly
            mode="folder"
            title={t("about.migratePickTitle")}
            openLabel={t("general.dataDir.open")}
            browseLabel={t("about.migrateBtn")}
            disabled={!!move}
            onOpen={() => void openDataDir()}
          />
        </Row>
      </Sec>

      {/* AI 能力：只读的能力陈述（内置工具 / 智能增强）。放这里而非设置——它是"告知"不是"配置"。 */}
      <Sec title={t("sc.capSec")}>
        <Row title={t("sc.builtinTools")} />
        <Row title={t("sc.webTools")} />
      </Sec>

      {FEATURES.autoUpdate && (
      <Sec title={t("about.updateSec")}>
        <Row title={t("about.checkUpdate")} wide>
          {upd.phase === "idle" && (
            <Button onClick={() => void checkUpdate()}>{t("about.checkUpdate")}</Button>
          )}
          {upd.phase === "checking" && (
            <Button variant="ghost" disabled>
              {t("about.checking")}
            </Button>
          )}
          {upd.phase === "upToDate" && <span className="about-val">{t("about.upToDate")}</span>}
          {upd.phase === "available" && (
            <div className="upd-row">
              <span className="about-val">{t("about.foundNew").replace("{v}", upd.version)}</span>
              <Button onClick={() => void installUpdate()}>{t("about.downloadInstall")}</Button>
            </div>
          )}
          {upd.phase === "installing" && (
            <span className="about-val">
              {t("about.installing").replace("{p}", String(upd.percent ?? 0))}
            </span>
          )}
          {upd.phase === "installed" && (
            <div className="upd-row">
              <span className="about-val">{t("about.installed")}</span>
              <Button onClick={() => void appApi.restart()}>{t("about.restartNow")}</Button>
            </div>
          )}
          {upd.phase === "error" && (
            <div className="upd-row">
              <span className="form-error">
                {t("about.updateErr")}
                {upd.msg}
              </span>
              <Button variant="ghost" onClick={() => void checkUpdate()}>
                {t("about.retry")}
              </Button>
            </div>
          )}
        </Row>
        {upd.phase === "available" && upd.body && (
          <pre className="upd-changelog">{upd.body}</pre>
        )}
      </Sec>
      )}

      {migrateTo && (
        <ConfirmDialog
          title={t("about.migrateConfirmTitle")}
          message={t("about.migrateConfirmMsg")
            .replace("{from}", info?.dataDir ?? "")
            .replace("{to}", migrateTo)}
          warning={t("about.migrateConfirmWarn")}
          okText={t("about.migrateConfirmOk")}
          onOk={() => void confirmMigrate()}
          onClose={() => setMigrateTo(null)}
        />
      )}

      {move && (
        <MigrationProgress
          target={move.target}
          progress={move.progress}
          done={move.done}
          error={move.error}
          onRestart={() => void appApi.restart()}
          onClose={() => setMove(null)}
        />
      )}

      <div className="about-foot">{t("about.copyright")}</div>
    </>
  );
}
