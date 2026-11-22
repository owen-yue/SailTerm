import { useEffect, useState } from "react";
import { baseName, formatBytes } from "../../lib/format";
import { transferActions, transferStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";
import type { OnConflict } from "../../types";

/**
 * 目标已存在时的裁决弹窗。
 *
 * 提交传输的默认策略是 `ask`，所以任何会撞名的传输都必然先经过这里——
 * **绝不闷头覆盖用户文件**。多个冲突排队逐个呈现，与主机密钥确认同一套交互习惯。
 * 选项只留「跳过 / 覆盖」；不做“改名另存”分支，避免弹窗堆一排选项。
 * 批量处理只加一个正文末尾的「应用到其余 N 项」复选框，
 * 而不是再堆一排「全部 xx」按钮（按钮数不随队列长度变，底栏永远只有三颗）。
 * 第三颗是「中止」（放弃剩余全部冲突），不是旧的「取消」——后者与「跳过」结果完全相同，
 * 因为后端 `Ask` 不挂起任务、不重提交就是不传这一项；故也不提供遮罩/Esc 关闭，
 * 避免误触时“关掉算哪一个”没有定论。
 */
export default function ConflictDialog() {
  const t = useI18n();
  const { conflicts } = useStore(transferStore);
  const c = conflicts[0];
  /** 勾选态：轮到下一条冲突时必须复位——否则上一次没提交的勾选会让后续的覆盖静默批量执行，
   *  破坏性动作不能继承上一次的隐式意图。 */
  const [applyAll, setApplyAll] = useState(false);
  useEffect(() => setApplyAll(false), [c?.src, c?.dstDir]);
  if (!c) return null;

  const choose = (choice: OnConflict) =>
    applyAll ? void transferActions.resolveAll(choice) : void transferActions.resolve(c, choice);

  return (
    <div className="modal-mask">
      <div className="modal conflict">
        <div className="modal-head">
          <h2>{t("dlg.conflictTitle")}</h2>
          {conflicts.length > 1 && (
            <span className="hk-more">{t("dlg.conflictMore").replace("{n}", String(conflicts.length - 1))}</span>
          )}
        </div>

        <div className="hk-body">
          <p>{t("dlg.conflictName").replace("{n}", baseName(c.src))}</p>
          <div className="hk-fp">{c.existingPath}</div>
          <div className="cf-sizes">
            <span>{t("dlg.conflictExists").replace("{n}", formatBytes(c.existingBytes))}</span>
            <span>{t("dlg.conflictThis").replace("{n}", c.srcBytes > 0 ? formatBytes(c.srcBytes) : t("dlg.conflictUnknown"))}</span>
          </div>
          {conflicts.length > 1 && (
            <label className="check">
              <input type="checkbox" checked={applyAll} onChange={(e) => setApplyAll(e.target.checked)} />
              {t("dlg.conflictApplyAll").replace("{n}", String(conflicts.length - 1))}
            </label>
          )}
        </div>

        <div className="form-actions">
          <button type="button" className="ghost" onClick={() => transferActions.abortAll()}>
            {t("dlg.conflictAbort")}
          </button>
          <button type="button" className="ghost" onClick={() => choose("skip")}>
            {t("dlg.skip")}
          </button>
          <button type="button" className="danger-soft" onClick={() => choose("overwrite")}>
            {t("dlg.overwrite2")}
          </button>
        </div>
      </div>
    </div>
  );
}
