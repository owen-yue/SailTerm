// 发送工具（独立工具窗口）：把编辑区里的脚本条目按「格式 × 分隔档 × 计数 × 间隔」发到目标会话的 PTY。
//
// 为什么是独立窗口而不是主窗口的停靠面板：脚本一份、目标多台，它的内容不属任何一条连接；
// 而停靠区（下栏 / 右栏）的开合是**按会话标签记忆**的——全局工具塞进去就得为它破例，
// 一条例外比没有规则更糟。所以 dock 的规矩一条不改，发送器住自己的窗口（关窗即在途发送停止）。
//
// 发送窗格布局：上方参数条，中间编辑区，右侧一列会话（勾谁发谁）。
// 会话按「侧栏连接分组」聚成组，分组样式与勾选控件与侧栏连接树同款（折叠三角 + 小方框）；同主机的多个会话靠 #n 序号区分。
// 会话列表自己会动：进窗口拉一次，之后靠窗口重新获得焦点 + 后端 `session-state` 跃迁补拉（无需手动刷新）。
//
// 安全红线：条目只写入用户自己的会话，不经 AI、不过策略引擎、不落盘（与在终端里手打同级）；
// Hex 档直发原始字节（含控制字符），用途是串口 / Telnet 设备调试，不做任何拦截也不做任何改写。
// 运行驱动在 sendStore（本组件只是它的一个视图），参数改动不会打断在途那一代。
import { useEffect, useMemo, useRef, useState } from "react";
import Button from "../../components/common/Button";
import StatusDot from "../../components/common/StatusDot";
import { useI18n } from "../../lib/i18n";
import { selfWindow, sessions as sshApi } from "../../lib/tauri";
import { useSessionName } from "../../lib/sessionName";
import {
  INFINITE,
  canSend,
  connectionStore,
  connections,
  sendActions,
  sendStore,
  sessionStore,
  useStore,
} from "../../stores";
import { MAX_COUNT, MIN_INTERVAL, parseEntries, type SendFormat, type SendSplit } from "../../lib/sendScript";
import { KIND_LABEL, groupTargetsByConnection } from "../../types";

/** 结果区每会话展示的终端近况行数（与窗口高度相称，多了反而看不清）。 */
const TAIL_LINES = 4;

/** 格式与分隔两组分段的取值表（驱动 `.seg` 按钮组，与认证方式切换同款）。 */
const FORMATS: SendFormat[] = ["text", "hex"];
const SPLITS: SendSplit[] = ["line", "pipe", "char"];

/**
 * 分组表头：折叠三角 + 整组勾选 + 组名 + 命中数。
 * 视觉直接复用侧栏连接树的 `.group-head / .caret / .gname`（同一套分组观感，不在工具窗口里另画一份）。
 * 两件事各自一个热区：点行 = 开合分组，点框 = 整组收/不收（stopPropagation 防互相误触）。
 * 三态（部分选中）没有属性可给，只能经 ref 设原生 checkbox 的 indeterminate。
 */
function SdGroupHead({
  name,
  total,
  hit,
  open,
  onOpen,
  onToggle,
}: {
  name: string;
  total: number;
  hit: number;
  open: boolean;
  onOpen: () => void;
  onToggle: (on: boolean) => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = hit > 0 && hit < total;
  }, [hit, total]);
  return (
    <div className="group-head" onClick={onOpen}>
      <span className="caret">{open ? "▾" : "▸"}</span>
      <input
        ref={ref}
        type="checkbox"
        checked={total > 0 && hit === total}
        onClick={(e) => e.stopPropagation()}
        onChange={(e) => onToggle(e.target.checked)}
      />
      <span className="gname" title={name}>
        {name}
      </span>
      <span className="gcount">
        {hit}/{total}
      </span>
    </div>
  );
}

export default function SendToolView() {
  const tr = useI18n();
  const s = useStore(sendStore);
  const { tabs } = useStore(sessionStore);
  const { profiles, groups } = useStore(connectionStore);
  const pool = useMemo(() => tabs.filter(canSend), [tabs]);
  // 重名消歧走全局唯一入口 `lib/sessionName`：基数是全量会话（不是本列可见的那几条），
  // 这样这里显 `#2` 的那条在页签上也是 `#2`。本列的代价是「往哪个会话灌命令」，对不上号就是往错机器上跑脚本。
  const name = useSessionName();
  // 分组口径与侧栏一致（会话 → 档案 → 分组，归不到的落「未分组」）。
  const grouped = useMemo(
    () => groupTargetsByConnection(pool, profiles, groups, tr("common.ungrouped")),
    [pool, profiles, groups, tr],
  );

  // 分组开合：跟侧栏一样只活在本次视图（不持久）；收起不影响已勾选的目标。
  const [collapsed, setCollapsed] = useState<string[]>([]);

  // 工具窗口是独立 JS 上下文：会话表与连接分组都得自己拉一份（主窗口的 store 不在这里）。
  // 三个时机：进窗口、窗口重新拿到焦点、后端会话状态跃迁——后两个让这一列不必手动刷新。
  // 焦点那条不是凑数：后端刻意不在初始建连发 state（结果直接随 `session_connect` 返回），
  // 所以在主窗口新开的会话在本窗口里没有事件可跟，只能等你点进来这一刻补拉一次。
  useEffect(() => {
    let cancelled = false;
    const offs: Array<() => void> = [];
    const keep = (u: () => void) => (cancelled ? u() : offs.push(u));
    const reload = () => {
      void connections.refresh();
      void sendActions.refresh();
    };
    reload();
    sshApi.onState(reload).then(keep);
    selfWindow
      .onFocusChanged((focused) => {
        if (focused) reload();
      })
      .then(keep);
    return () => offs.forEach((u) => u());
  }, []);

  // 实时解析：条目数与 Hex 合法性边写边校，别等按下「开始」才告诉用户第 3 行不是十六进制。
  const plan = useMemo(
    () => parseEntries(s.text, s.format, s.split, s.appendEnter),
    [s.text, s.format, s.split, s.appendEnter],
  );

  const running = s.running;
  const rounds = s.count === INFINITE ? "∞" : String(s.count);
  const status = running
    ? tr("sd.running").replace("{r}", String(s.round)).replace("{c}", rounds)
      + ` · ${tr("sd.entry").replace("{i}", String(s.entry)).replace("{n}", String(s.total))}`
      + ` · ${tr("sd.nTargets").replace("{n}", String(s.targets.length))}`
    : plan.error || plan.entries.length === 0
      ? "" // 空编辑区不是错误：显「待命」就够，开始按钮本来就是灰的
      : tr("sd.entries").replace("{n}", String(plan.entries.length));
  // 就地错误：启动失败用 store 里的串，解析非法用实时解析的结果（两者都显示在参数条下方）。
  const parseErr =
    plan.error === null ? "" : tr(plan.error.key) + (plan.error.at === undefined ? "" : ` (${plan.error.at + 1})`);
  const err = s.error || parseErr;
  const canStart = !running && plan.error === null && plan.entries.length > 0;

  /** 会发给谁：勾选集合 ∩ 当前会话池（存量里已关掉的会话自然落空）。 */
  const hit = useMemo(() => new Set(pool.filter((x) => s.picked.includes(x.id)).map((x) => x.id)), [pool, s.picked]);
  const byId = useMemo(() => new Map(pool.map((x) => [x.id, x])), [pool]);
  const allIds = pool.map((x) => x.id);

  return (
    <div className="sd">
      <div className="sd-bar">
        {running ? (
          <Button variant="outline" danger size="sm" onClick={() => sendActions.stop()}>
            <span className="sd-glyph">■</span>
            {tr("sd.stop")}
          </Button>
        ) : (
          <Button size="sm" disabled={!canStart} title={tr("sd.startTip")} onClick={() => void sendActions.start()}>
            <span className="sd-glyph">▶</span>
            {tr("sd.start")}
          </Button>
        )}
        <span className="sd-stat" data-run={running}>
          {status || tr("sd.idle")}
        </span>
      </div>

      <div className="sd-bar sd-opts">
        <div className="seg seg-sm" role="radiogroup" aria-label={tr("sd.format")}>
          {FORMATS.map((v) => (
            <button
              key={v}
              type="button"
              role="radio"
              aria-checked={s.format === v}
              data-on={s.format === v}
              onClick={() => sendActions.set({ format: v })}
            >
              {tr(`sd.format.${v}`)}
            </button>
          ))}
        </div>

        <div className="seg seg-sm" role="radiogroup" aria-label={tr("sd.split")}>
          {SPLITS.map((v) => (
            <button
              key={v}
              type="button"
              role="radio"
              aria-checked={s.split === v}
              data-on={s.split === v}
              title={tr(`sd.split.${v}.hint`)}
              onClick={() => sendActions.set({ split: v })}
            >
              {tr(`sd.split.${v}`)}
            </button>
          ))}
        </div>

        {s.split !== "char" && (
          <label className="check">
            <input
              type="checkbox"
              checked={s.appendEnter}
              onChange={(e) => sendActions.set({ appendEnter: e.target.checked })}
            />
            {tr("sd.appendEnter")}
          </label>
        )}

        <label className="sd-field">
          {tr("sd.count")}
          <input
            className="sd-num"
            type="number"
            min={INFINITE}
            max={MAX_COUNT}
            step={1}
            value={s.count}
            disabled={s.count === INFINITE}
            onChange={(e) => sendActions.set({ count: Number(e.target.value) })}
          />
          <button
            type="button"
            className="sd-inf"
            data-on={s.count === INFINITE}
            title={tr("sd.infinite")}
            onClick={() => sendActions.set({ count: s.count === INFINITE ? 1 : INFINITE })}
          >
            ∞
          </button>
        </label>

        <label className="sd-field">
          {tr("sd.interval")}
          <input
            className="sd-num"
            type="number"
            min={MIN_INTERVAL}
            max={3600}
            step={0.05}
            value={s.interval}
            onChange={(e) => sendActions.set({ interval: Number(e.target.value) })}
          />
          <span className="sd-unit">{tr("sd.sec")}</span>
        </label>
      </div>

      {err && <div className="form-error">{err}</div>}

      <div className="sd-main">
        <textarea
          className="sd-editor"
          value={s.text}
          spellCheck={false}
          placeholder={tr(s.format === "hex" ? "sd.editorPhHex" : "sd.editorPhText")}
          onChange={(e) => sendActions.set({ text: e.target.value })}
        />

        <div className="sd-side">
          <div className="sd-side-head">
            {tr("sd.targets")}
            <span className="ss-count">{hit.size}/{pool.length}</span>
            {pool.length > 0 && (
              <>
                <button type="button" className="link" onClick={() => sendActions.set({ picked: allIds })}>
                  {tr("sd.selectAll")}
                </button>
                <button
                  type="button"
                  className="link"
                  disabled={s.picked.length === 0}
                  onClick={() => sendActions.set({ picked: [] })}
                >
                  {tr("sd.clearAll")}
                </button>
              </>
            )}
          </div>

          <div className="sd-list">
            {pool.length === 0 ? (
              <div className="ss-empty">{tr("sd.empty")}</div>
            ) : (
              grouped.map((g) => {
                const open = !collapsed.includes(g.id);
                return (
                  <section key={g.id || "__ungrouped"} className="sd-group">
                    <SdGroupHead
                      name={g.name}
                      total={g.tabIds.length}
                      hit={g.tabIds.filter((id) => hit.has(id)).length}
                      open={open}
                      onOpen={() =>
                        setCollapsed((prev) =>
                          prev.includes(g.id) ? prev.filter((x) => x !== g.id) : [...prev, g.id],
                        )
                      }
                      onToggle={(on) => sendActions.setGroupPicked(g.tabIds, on)}
                    />
                    {open &&
                      g.tabIds.map((id) => {
                        const tab = byId.get(id);
                        if (!tab) return null;
                        return (
                          <label key={id} className="conn">
                            <input
                              type="checkbox"
                              checked={hit.has(id)}
                              onChange={() => sendActions.togglePicked(id)}
                            />
                            <StatusDot state={tab.state} tooltip={false} />
                            <span className="meta">
                              <span className="nm" title={tab.host}>
                                {name(tab)}
                              </span>
                            </span>
                            <span className="sd-kind">{tr(KIND_LABEL[tab.kind])}</span>
                          </label>
                        );
                      })}
                  </section>
                );
              })
            )}
          </div>

          {s.targets.length > 0 && (
            <div className="sd-res">
              <div className="sd-res-head">{tr("sd.results")}</div>
              {s.targets.map((x) => {
                const tail = (s.tails[x.id] ?? "").split("\n").slice(-TAIL_LINES).join("\n");
                return (
                  <div key={x.id} className="sd-row" data-ok={x.error === ""}>
                    <span className="sd-label">{x.label}</span>
                    <span className="sd-detail">{x.error || tr("sd.sent").replace("{n}", String(x.sent))}</span>
                    {tail && <pre className="sd-tail">{tail}</pre>}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
