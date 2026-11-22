import { useEffect, useMemo, useState } from "react";
import { IconEdit, IconRefresh, IconTunnel } from "../common/icons";
import { copyText } from "../../lib/clipboard";
import { connectionStore, sessionStore, tunnelActions, tunnelStore, useStore } from "../../stores";
import { TUNNEL_KIND_LABEL, sessionLabel as getSessionLabel } from "../../types";
import type { TunnelConfig, TunnelInput, TunnelKind } from "../../types";
import Sheet from "../common/Sheet";
import Popconfirm from "../common/Popconfirm";
import DeleteButton from "../common/DeleteButton";
import Select from "../common/Select";
import { useI18n } from "../../lib/i18n";
import { hasCap } from "../../lib/capabilities";

interface Props {
  onClose: () => void;
  /** 嵌入设置中心时不套模态壳。 */
  embedded?: boolean;
}

/** 类型下拉的顺序：本地 -L / 远程 -R / 动态 SOCKS -D。 */
const KIND_ORDER: TunnelKind[] = ["local", "remote", "dynamic"];

/** 按 kind 决定表单两组字段的语义标签。 */
function fieldLabels(kind: TunnelKind, t: (k: string) => string) {
  if (kind === "remote") {
    // 存储语义：local_* = 回连的本地目标，remote_* = 远端监听。
    return {
      leftHost: t("tm.hostLocalTarget"),
      leftPort: t("tm.portLocalTarget"),
      leftHostPh: "127.0.0.1",
      leftPortPh: "3000",
      rightHost: t("tm.hostRemoteListen"),
      rightPort: t("tm.portRemoteListen"),
      rightHostPh: "0.0.0.0 / server",
      rightPortPh: t("tm.phZeroAlloc"),
    };
  }
  if (kind === "dynamic") {
    return {
      leftHost: t("tm.hostSocks"),
      leftPort: t("tm.portSocks"),
      leftHostPh: "127.0.0.1",
      leftPortPh: t("tm.phEmptyRandom"),
      rightHost: "",
      rightPort: "",
      rightHostPh: "",
      rightPortPh: "",
    };
  }
  return {
    leftHost: t("tm.hostLocalListen"),
    leftPort: t("tm.portLocalListen"),
    leftHostPh: "127.0.0.1",
    leftPortPh: t("tm.phEmptyRandom"),
    rightHost: t("tm.hostRemoteTarget"),
    rightPort: t("tm.portRemoteTarget"),
    rightHostPh: "127.0.0.1 / db.internal",
    rightPortPh: "5432",
  };
}

/** 运行中/规则库的路由一行：按 kind 决定箭头方向与文案。 */
function RouteLine({ kind, localHost, localPort, remoteHost, remotePort }: {
  kind: TunnelKind | string;
  localHost: string;
  localPort: number;
  remoteHost: string;
  remotePort: number;
}) {
  const t = useI18n();
  const lp = localPort === 0 ? t("tm.random") : String(localPort);
  if (kind === "dynamic") {
    return (
      <div className="tun-route">
        <code>
          {localHost}:{lp}
        </code>
        <span className="tun-badge">SOCKS5</span>
      </div>
    );
  }
  if (kind === "remote") {
    return (
      <div className="tun-route">
        <code>
          R {remoteHost}:{remotePort === 0 ? t("tm.random") : remotePort}
        </code>
        <span className="tun-arrow">→</span>
        <code>
          {localHost}:{lp}
        </code>
      </div>
    );
  }
  return (
    <div className="tun-route">
      <code>
        L {localHost}:{lp}
      </code>
      <span className="tun-arrow">→</span>
      <code>
        {remoteHost}:{remotePort}
      </code>
    </div>
  );
}

/**
 * 🔀 SSH 隧道 / 端口转发模态：管理本地端口转发规则，并在**当前已连接的
 * SSH 会话**上一键开启 / 关闭转发。上半区是「运行中的转发」，下半区是「转发规则库」。
 *
 * 安全红线：规则只存**地址与端口**，绝不含凭证；开启时复用会话已认证的连接开
 * `direct-tcpip`，**不再索取密码 / 私钥**。会话断开时后端 `teardown` 会自动关闭其名下所有隧道。
 */
export default function TunnelManager({ onClose, embedded = false }: Props) {
  const t = useI18n();
  const { configs, active, loading, error } = useStore(tunnelStore);
  const { profiles } = useStore(connectionStore);
  const { tabs, activeId } = useStore(sessionStore);

  const [query, setQuery] = useState("");
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<TunnelConfig | null>(null);
  /** 待删除的规则 + 气泡锚点：与侧栏删连接同口径的就地确认。 */
  const [pendingDel, setPendingDel] = useState<{ id: string; name: string; x: number; y: number } | null>(null);

  // 新建 / 编辑表单（内联展开，新建 / 编辑复用同一份）
  const [fName, setFName] = useState("");
  const [fKind, setFKind] = useState<TunnelKind>("local");
  const labels = fieldLabels(fKind, t);
  const [fLocalHost, setFLocalHost] = useState("127.0.0.1");
  const [fLocalPort, setFLocalPort] = useState("");
  const [fRemoteHost, setFRemoteHost] = useState("");
  const [fRemotePort, setFRemotePort] = useState("");
  const [fProfile, setFProfile] = useState("");
  const [fEnabled, setFEnabled] = useState(true);
  const [localErr, setLocalErr] = useState("");
  const [copied, setCopied] = useState("");

  useEffect(() => {
    void tunnelActions.refresh();
    void tunnelActions.refreshActive();
  }, []);

  /** 隧道依附的会话：优先当前活动标签（若为已连接 SSH），否则退回任一已连接 SSH 会话。 */
  const activeSsh = useMemo(() => {
    const a = tabs.find((t) => t.id === activeId);
    if (a && hasCap(a.kind, "tunnel") && a.state === "connected") return a;
    return tabs.find((t) => hasCap(t.kind, "tunnel") && t.state === "connected") ?? null;
  }, [tabs, activeId]);

  /** session_id → 标签，供运行中列表展示所属会话。 */
  const sessionLabel = useMemo(() => {
    const m = new Map<string, string>();
    for (const tab of tabs) m.set(tab.id, getSessionLabel(tab, t));
    return m;
  }, [tabs, t]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return configs;
    return configs.filter(
      (c) =>
        c.name.toLowerCase().includes(q) ||
        c.remoteHost.toLowerCase().includes(q) ||
        String(c.remotePort).includes(q),
    );
  }, [configs, query]);

  function openNew() {
    setEditing(null);
    setFName("");
    setFKind("local");
    setFLocalHost("127.0.0.1");
    setFLocalPort("");
    setFRemoteHost("");
    setFRemotePort("");
    setFProfile(activeSsh?.profileId ?? "");
    setFEnabled(true);
    setLocalErr("");
    setFormOpen(true);
  }

  function openEdit(c: TunnelConfig) {
    setEditing(c);
    setFName(c.name);
    setFKind(c.kind);
    setFLocalHost(c.localHost);
    setFLocalPort(c.localPort === 0 ? "" : String(c.localPort));
    setFRemoteHost(c.remoteHost);
    setFRemotePort(String(c.remotePort));
    setFProfile(c.profileId ?? "");
    setFEnabled(c.enabled);
    setLocalErr("");
    setFormOpen(true);
  }

  async function submitForm() {
    if (!fName.trim()) return setLocalErr(t("tm.errName"));
    const isDyn = fKind === "dynamic";
    const isRemote = fKind === "remote";
    if (!isDyn && !fRemoteHost.trim()) return setLocalErr(t("tm.errHost"));
    const rp = isDyn ? 0 : Number(fRemotePort);
    if (!isDyn && !Number.isInteger(rp)) return setLocalErr(t("tm.errPortInt"));
    // -L 远端目标端口必填；-R 远端监听端口 0 = 服务器分配
    if (!isDyn && !isRemote && (rp < 1 || rp > 65535))
      return setLocalErr(t("tm.errRemoteTargetRange"));
    if (isRemote && (rp < 0 || rp > 65535))
      return setLocalErr(t("tm.errRemoteListenRange"));
    const lpRaw = fLocalPort.trim();
    const lp = lpRaw === "" ? 0 : Number(lpRaw);
    if (!Number.isInteger(lp) || lp < 0 || lp > 65535)
      return setLocalErr(t("tm.errPortRange"));
    // -R 的 local_* 是回连目标端口，必须真实可达，不能随机
    if (fKind === "remote" && lp === 0) return setLocalErr(t("tm.errLocalTargetRequired"));
    setLocalErr("");
    const input: TunnelInput = {
      ...(editing ? { id: editing.id } : {}),
      name: fName.trim(),
      kind: fKind,
      localHost: fLocalHost.trim() || "127.0.0.1",
      localPort: lp,
      remoteHost: isDyn ? "" : fRemoteHost.trim(),
      remotePort: rp,
      profileId: fProfile || null,
      enabled: fEnabled,
    };
    const saved = await tunnelActions.save(input);
    if (saved) {
      setFormOpen(false);
      setEditing(null);
    }
  }

  async function onCopy(text: string) {
    if (await copyText(text)) {
      setCopied(text);
      setTimeout(() => setCopied(""), 1200);
    }
  }

  const err = localErr || error;
  const profileName = (id: string | null) => (id ? (profiles.find((p) => p.id === id)?.name ?? id) : "");

  return (
    <>
    <Sheet title={<><IconTunnel /> {t("tm.title")}</>} onClose={onClose} embedded={embedded}>
        <div className="sec-set-body">
          {/* 运行中的转发 */}
          <section className="ss-sec">
            <div className="ss-sec-head">
              {t("tm.running")}
              <span className="ss-count">{active.length}</span>
              <button
                type="button"
                className="icon-btn"
                title={t("common.refresh")}
                aria-label={t("common.refresh")}
                onClick={() => void tunnelActions.refreshActive()}
              >
                <IconRefresh />
              </button>
            </div>
            {!activeSsh && (
              <div className="form-hint">{t("tm.needSsh")}</div>
            )}
            {active.length === 0 ? (
              <div className="ss-empty">
                {t("tm.emptyRunning")}
              </div>
            ) : (
              <div className="snip-list">
                {active.map((a) => (
                  <div key={a.runId} className="snip-card tun-active">
                    <div className="snip-main">
                      <div className="snip-top">
                        <span className="snip-name">{a.name}</span>
                        <span className="tun-badge">{t("tm.badgeRunning")}</span>
                      </div>
                      <RouteLine
                        kind={a.kind ?? "local"}
                        localHost={a.localHost}
                        localPort={a.localPort}
                        remoteHost={a.remoteHost}
                        remotePort={a.remotePort}
                      />
                      <div className="snip-desc">{t("tm.session")}{sessionLabel.get(a.sessionId) ?? a.sessionId}</div>
                    </div>
                    <div className="snip-side">
                      <button
                        type="button"
                        className="snip-insert"
                        title={t("tm.copyAddrTitle")}
                        onClick={() => void onCopy(`${a.localHost}:${a.localPort}`)}
                      >
                        {copied === `${a.localHost}:${a.localPort}` ? t("tm.copied") : t("tm.copyAddr")}
                      </button>
                      <div className="snip-acts">
                        <button type="button" className="link danger" onClick={() => void tunnelActions.close(a.runId)}>
                          {t("common.close")}
                        </button>
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>

          {/* 转发规则库 */}
          <section className="ss-sec">
            <div className="ss-sec-head">
              {t("tm.ruleLib")}
              <span className="ss-count">{configs.length}</span>
              {!formOpen && (
                <button type="button" className="link" onClick={openNew}>
                  ＋ {t("tm.newRule")}
                </button>
              )}
            </div>

            <div className="snip-toolbar">
              <input
                className="snip-search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("tm.searchPh")}
                spellCheck={false}
              />
            </div>

            <div className="form-hint">{t("tm.hintNoCred")}</div>

            {loading && <div className="ss-empty">{t("common.loading")}</div>}
            {!loading && visible.length === 0 && !formOpen && (
              <div className="ss-empty">
                <div className="ss-empty">{configs.length === 0 ? t("tm.noRule") : t("common.noMatch")}</div>
              </div>
            )}

            <div className="snip-list">
              {visible.map((c) => {
                const canOpen = !!activeSsh;
                return (
                  <div key={c.id} className="snip-card">
                    <div className="snip-main">
                      <div className="snip-top">
                        <span className="snip-name">{c.name}</span>
                        <span className="snip-group">{t(TUNNEL_KIND_LABEL[c.kind])}</span>
                        {!c.enabled && <span className="tun-off">{t("common.disabled")}</span>}
                      </div>
                      <RouteLine
                        kind={c.kind}
                        localHost={c.localHost}
                        localPort={c.localPort}
                        remoteHost={c.remoteHost}
                        remotePort={c.remotePort}
                      />
                      {c.profileId && <div className="snip-desc">{t("tm.bindConn")}{profileName(c.profileId)}</div>}
                    </div>
                    <div className="snip-side">
                      <button
                        type="button"
                        className="snip-insert"
                        disabled={!canOpen}
                        title={activeSsh ? t("tm.openTitle") : t("tm.openNeedTitle")}
                        onClick={() => void tunnelActions.open(c.id, activeSsh?.id)}
                      >
                        {t("tm.open")}
                      </button>
                      <div className="snip-acts">
                        <button
                          type="button"
                          className="icon-btn"
                          title={t("common.edit")}
                          aria-label={t("common.edit")}
                          onClick={() => openEdit(c)}
                        >
                          <IconEdit />
                        </button>
                        <DeleteButton onConfirm={(x, y) => setPendingDel({ id: c.id, name: c.name, x, y })} />
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>

            {formOpen && (
              <form
                className="ss-form"
                onSubmit={(e) => {
                  e.preventDefault();
                  void submitForm();
                }}
              >
                <label>
                  {t("common.name")}
                  <input
                    value={fName}
                    onChange={(e) => setFName(e.target.value)}
                    placeholder={t("tm.namePh")}
                    autoFocus
                    spellCheck={false}
                  />
                </label>
                <label>
                  {t("tm.type")}
                  <Select
                    className="field"
                    value={fKind}
                    options={KIND_ORDER.map((k) => ({ value: k, label: t(TUNNEL_KIND_LABEL[k]) }))}
                    onChange={(v) => setFKind(v as TunnelKind)}
                  />
                </label>
                <div className="tun-row">
                  <label>
                    {labels.leftHost}
                    <input
                      value={fLocalHost}
                      onChange={(e) => setFLocalHost(e.target.value)}
                      placeholder={labels.leftHostPh}
                      spellCheck={false}
                    />
                  </label>
                  <label>
                    {labels.leftPort}
                    <input
                      value={fLocalPort}
                      onChange={(e) => setFLocalPort(e.target.value)}
                      placeholder={labels.leftPortPh}
                      inputMode="numeric"
                      spellCheck={false}
                    />
                  </label>
                </div>
                {fKind !== "dynamic" && (
                  <div className="tun-row">
                    <label>
                      {labels.rightHost}
                      <input
                        value={fRemoteHost}
                        onChange={(e) => setFRemoteHost(e.target.value)}
                        placeholder={labels.rightHostPh}
                        spellCheck={false}
                      />
                    </label>
                    <label>
                      {labels.rightPort}
                      <input
                        value={fRemotePort}
                        onChange={(e) => setFRemotePort(e.target.value)}
                        placeholder={labels.rightPortPh}
                        inputMode="numeric"
                        spellCheck={false}
                      />
                    </label>
                  </div>
                )}
                <label>
                  {t("tm.bindLabel")}
                  <Select
                    className="field"
                    value={fProfile}
                    options={[
                      { value: "", label: t("tm.bindNone") },
                      ...profiles.map((p) => ({ value: p.id, label: p.name })),
                    ]}
                    onChange={(v) => setFProfile(v)}
                  />
                </label>
                <label className="tun-check">
                  <input type="checkbox" checked={fEnabled} onChange={(e) => setFEnabled(e.target.checked)} />
                  {t("tm.enableRule")}
                </label>
                <div className="form-actions">
                  <button type="button" className="ghost" onClick={() => setFormOpen(false)}>
                    {t("common.cancel")}
                  </button>
                  <button type="submit">{editing ? t("tm.saveEdit") : t("tm.addRule")}</button>
                </div>
              </form>
            )}
          </section>
        </div>

        {err && <div className="form-error">{err}</div>}

        <div className="form-actions sec-set-foot">
          <span className="form-hint">{t("tm.hintTeardown")}</span>
          {!embedded && (
            <button type="button" onClick={onClose}>
              {t("tm.finish")}
            </button>
          )}
        </div>
    </Sheet>

    {pendingDel && (
      <Popconfirm
        x={pendingDel.x}
        y={pendingDel.y}
        title={t("tm.delMsg").replace("{n}", pendingDel.name)}
        description={t("tm.delWarn")}
        onConfirm={() => {
          const id = pendingDel.id;
          setPendingDel(null);
          void tunnelActions.remove(id);
        }}
        onClose={() => setPendingDel(null)}
      />
    )}
    </>
  );
}
