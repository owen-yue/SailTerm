import { useI18n } from "../../lib/i18n";
import type { MoveProgress } from "../../lib/tauri";

interface Props {
  /** 迁移目标目录（展示用）。 */
  target: string;
  /** 最近一次进度事件；未开始为 null。 */
  progress: MoveProgress | null;
  /** 迁移成功完成。 */
  done: boolean;
  /** 迁移失败原因；无则 null。 */
  error: string | null;
  /** 立即重启（仅完成后提供）。 */
  onRestart: () => void;
  /** 关闭弹框（仅完成/失败后可用）。 */
  onClose: () => void;
}

/**
 * 数据目录迁移进度弹框：运行中不可关闭（无 ×、点遮罩无效），
 * 完成或失败后才允许关闭；完成时给出「立即重启」以切到新目录。
 */
export default function MigrationProgress({
  target,
  progress,
  done,
  error,
  onRestart,
  onClose,
}: Props) {
  const t = useI18n();
  const running = !done && !error;
  const pct = done ? 100 : (progress?.percent ?? 0);
  const phase = progress?.phase ?? "files";
  const label = error
    ? t("about.migrateFail")
    : done
      ? t("about.migratePhaseDone")
      : phase === "database"
        ? t("about.migratePhaseDb")
        : `${t("about.migratePhaseFiles")}${
            progress && progress.total > 0 ? ` (${progress.done}/${progress.total})` : ""
          }`;
  return (
    <div className="modal-mask">
      <div className="modal prompt">
        <div className="modal-head">
          <h2>{t("about.migrateConfirmTitle")}</h2>
          {!running && (
            <button type="button" className="modal-x" title={t("common.close")} onClick={onClose}>
              ×
            </button>
          )}
        </div>

        <div className="hk-body">
          <p className="cf-msg">{target}</p>
          <div className="mp-bar">
            <div
              className={`mp-fill${error ? " err" : done ? " ok" : ""}`}
              style={{ width: `${pct}%` }}
            />
          </div>
          <div className="mp-label">
            <span>{label}</span>
            <span className="mp-pct">{pct}%</span>
          </div>
          {running && progress?.current && <div className="mp-cur">{progress.current}</div>}
          {running && <p className="migrate-hint">{t("about.migrateRunningHint")}</p>}
          {error && <p className="hk-warn">{error}</p>}
          {done && (
            <p className="migrate-done">{t("about.migrateDoneDesc").replace("{to}", target)}</p>
          )}
        </div>

        <div className="form-actions">
          {error ? (
            <button type="button" className="ghost" onClick={onClose}>
              {t("common.close")}
            </button>
          ) : done ? (
            <>
              <button type="button" className="ghost" onClick={onClose}>
                {t("about.migrateLater")}
              </button>
              <button type="button" onClick={onRestart}>
                {t("about.migrateRestart")}
              </button>
            </>
          ) : (
            <button type="button" disabled>
              {t("about.migrating")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
