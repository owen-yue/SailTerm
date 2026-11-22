import { useEffect, useRef, useState } from "react";
import { errMsg } from "../../lib/tauri";
import { useI18n } from "../../lib/i18n";
import { lockActions, lockStore, useStore } from "../../stores";
import { toast } from "../../stores/toastStore";
import Button from "./Button";

/**
 * 锁屏遮罩：App 根级全屏不透明层（见 main.tsx，三根共用一个挂载点）。
 *
 * 两条实现纪律（改动前务必读）：
 * 1. **盖住、不卸载**：锁定期间底下组件全部保持挂载（传输/AI 继续跑、终端缓冲不丢），
 *    解锁即原位恢复。绝不改成「locked 时条件渲染工作区」——卸载会销毁 xterm 滚动缓冲。
 * 2. **盖住顶栏工具条，但窗口控件浮于其上**：本层 z-index 500 盖住整块工作区与顶栏功能
 *    按钮（命令面板/本地终端/发送/设置等锁定态一律不可点），唯 `.win-controls`（缩小/最大化/
 *    关闭）以 z-index 600 浮到遮罩之上，锁定态仍可正常操作窗口。关闭即结束当前会话：本锁是
 *    「防临时窥屏」的会话级闸门，重开不自动上锁（产品明确取舍，非启动门禁）。
 *
 * 输入闸门（快捷键/右键/拖放吞弃）在 lib/lockClient 的 window 层统一处理；本组件只
 * 负责遮罩自身：PIN 输入、退避倒计时、失败提示。PIN 明文只作 unlock 入参瞬时过手。
 */
export default function LockOverlay() {
  const { locked } = useStore(lockStore);
  const t = useI18n();
  const [pin, setPin] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  /** 输入框抖动动画标志（错误出现时触发，动画结束自动复位）。 */
  const [shaking, setShaking] = useState(false);
  /** 退避锁止倒计时（秒）；>0 期间禁提交，到 0 自动放行重试。 */
  const [wait, setWait] = useState(0);
  /** 「忘记密码」答题面板开合（重置必须答对密保，不做无验证的一键清除）。 */
  const [forgot, setForgot] = useState(false);
  /** 后端拉回的密保问题原文（null=未配/拉取失败；非空=已配、可答题）。 */
  const [question, setQuestion] = useState<string | null>(null);
  /** 问题拉取是否已完成（区分「加载中」与「确无密保」，避免面板闪现降级文案）。 */
  const [qReady, setQReady] = useState(false);
  const [answer, setAnswer] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const answerRef = useRef<HTMLInputElement>(null);

  // 每次落锁重置局部表单并抢焦点（多窗口场景本窗可能不在前台，focus 拿不到也无害——
  // 用户点进遮罩即落在输入框）。
  useEffect(() => {
    if (!locked) return;
    setPin("");
    setError("");
    setBusy(false);
    setWait(0);
    setForgot(false);
    setQuestion(null);
    setQReady(false);
    setAnswer("");
    inputRef.current?.focus();
  }, [locked]);

  useEffect(() => {
    if (wait <= 0) return;
    const id = window.setInterval(() => setWait((w) => Math.max(0, w - 1)), 1000);
    return () => window.clearInterval(id);
  }, [wait]);

  if (!locked) return null;

  async function submit() {
    if (busy || wait > 0 || !pin) return;
    setBusy(true);
    setError("");
    try {
      const out = await lockActions.unlock(pin);
      if (out.ok) {
        setPin(""); // 撤罩由 lock-changed 事件驱动 store 翻转，这里不自行翻态
      } else if (out.blocked) {
        setWait(out.retryAfterSecs);
      } else {
        setError(t("lock.wrongPin"));
        setShaking(true);
        setPin("");
        inputRef.current?.focus();
      }
    } catch (e) {
      // IPC 兜底超时（60s）等真异常：文案直显，输入不清（用户可能想原样重发）
      setError(errMsg(e));
      setShaking(true);
    } finally {
      setBusy(false);
    }
  }

  /**
   * 打开「忘记密码」答题面板：同时拉回密保问题展示。问题非敏感，后端直返原文；
   * 拉回 null（遗留 PIN 未配密保）则面板降级为“请改用旧 PIN 去设置页关闭”。
   */
  async function openForgot() {
    setForgot(true);
    setError("");
    setAnswer("");
    setQuestion(null);
    setQReady(false);
    try {
      const q = await lockActions.secqQuestion();
      setQuestion(q);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setQReady(true);
    }
    answerRef.current?.focus();
  }

  /**
   * 忘记 PIN 的找回：向后端提交密保答案，答对才精确删锁屏 PIN 单条并解锁（绝不误伤其他连接凭证）。
   * 预期结果走返回值：退避→复用主卡的 wait 起倒计时，答错/无密保→就地文案（不弹回解锁页）。
   */
  async function doReset() {
    if (busy || wait > 0 || !answer.trim()) return;
    setBusy(true);
    setError("");
    try {
      const out = await lockActions.resetPin(answer);
      if (out.ok) {
        // 成功：store 经回包翻锁，遮罩自行消失（locked 变 false），无需手动收面板。
        toast.show(t("lock.resetDone"));
        return;
      }
      if (out.blocked) {
        setWait(out.retryAfterSecs);
      } else if (out.noSecq) {
        setError(t("err.lock.noSecq"));
      } else {
        setError(t("err.lock.wrongAnswer"));
      }
      setAnswer("");
      answerRef.current?.focus();
    } catch (e) {
      // 真 IPC/IO 异常才落这里：文案直显，输入不清（用户可能想原样重发）。
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  // 「忘记密码」答题面板：与解锁卡同处遮罩内（z500 之下全局模态会被盖，故不走 FormDialog）。
  if (forgot) {
    const hasSecq = qReady && !!question;
    return (
      <div className="lock-overlay" onContextMenu={(e) => e.preventDefault()}>
        <form
          className="lock-card"
          onSubmit={(e) => {
            e.preventDefault();
            void doReset();
          }}
          onKeyDown={(e) => {
            if (e.key === "Tab") {
              e.preventDefault();
              answerRef.current?.focus();
            }
          }}
        >
          <h2 className="lock-title">{t("lock.forgotTitle")}</h2>
          {!qReady ? (
            <p className="lock-hint">{t("lock.secqLoading")}</p>
          ) : hasSecq ? (
            <>
              <p className="lock-hint">{t("lock.forgotBody")}</p>
              <div className="lock-secq-q">{question}</div>
              <input
                ref={answerRef}
                className="lock-answer"
                type="text"
                autoComplete="off"
                maxLength={100}
                value={answer}
                onChange={(e) => setAnswer(e.target.value)}
                placeholder={t("lock.secqAnswerPh")}
                aria-label={t("lock.secqAnswerLabel")}
                autoFocus
              />
            </>
          ) : (
            <p className="lock-hint">{t("lock.noSecqNote")}</p>
          )}
          <div className={`lock-err${(wait > 0 || error) ? " visible" : ""}`}>
            {wait > 0
              ? t("lock.blocked").replace("{n}", String(wait))
              : error}
          </div>
          <div className="lock-actions">
            <Button variant="ghost" disabled={busy} onClick={() => setForgot(false)}>
              {t("common.back")}
            </Button>
            {hasSecq && (
              <Button
                type="submit"
                variant="outline"
                size="sm"
                danger
                loading={busy}
                disabled={!answer.trim() || wait > 0}
              >
                {t("lock.forgotConfirm")}
              </Button>
            )}
          </div>
        </form>
      </div>
    );
  }

  return (
    <div className="lock-overlay" onContextMenu={(e) => e.preventDefault()}>
      {/* Tab 微焦点圈：遮罩内只有输入框与忘记密码链接，拦下 Tab 让它留在输入框，
          不给键盘绕到背景元素的机会（背景指针已被遮罩物理挡住）。解锁无独立按钮，输入框回车即提交。 */}
      <form
        className="lock-card"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
        onKeyDown={(e) => {
          if (e.key === "Tab") {
            e.preventDefault();
            inputRef.current?.focus();
          }
        }}
      >
        <svg
          className="lock-ico"
          width="26"
          height="26"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <rect x="4" y="10" width="16" height="11" rx="2.5" />
          <path d="M8 10V7a4 4 0 0 1 8 0v3" />
          <circle cx="12" cy="15.5" r="1.4" fill="currentColor" stroke="none" />
        </svg>
        <input
          ref={inputRef}
          className={`lock-pin${error ? " err" : ""}${shaking ? " shake" : ""}`}
          type="password"
          inputMode="numeric"
          autoComplete="off"
          maxLength={6}
          value={pin}
          onChange={(e) => setPin(e.target.value.replace(/\D/g, ""))}
          placeholder={t("lock.pinPh")}
          aria-label={t("lock.title")}
          autoFocus
          onAnimationEnd={() => setShaking(false)}
        />
        <div className={`lock-err${(wait > 0 || error) ? " visible" : ""}`}>
          {wait > 0
            ? t("lock.blocked").replace("{n}", String(wait))
            : error}
        </div>
        <button type="button" className="lock-forgot" onClick={() => void openForgot()}>
          {t("lock.forgot")}
        </button>
      </form>
    </div>
  );
}
