import { useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { app as appApi } from "../../lib/tauri";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";
import {
  appearanceActions,
  memoryActions,
  providerActions,
  settingsActions,
  snippetActions,
  sshKeyActions,
  terminalActions,
  triggerActions,
  tunnelActions,
} from "../../stores";
import type { AppInfo } from "../../types";
import { useI18n } from "../../lib/i18n";
import GeneralSettings from "./GeneralSettings";
import ProviderSettings from "./ProviderSettings";
import AiPrefs from "./AiPrefs";
import PromptTemplatePanel from "../ai/PromptTemplatePanel";
import MemoryManager from "../ai/MemoryManager";
import TerminalFont from "../terminal/TerminalFont";
import TerminalPrefs from "./TerminalPrefs";
import TriggerManager from "../terminal/TriggerManager";
import AppearanceSettings from "./AppearanceSettings";
import SecuritySettings from "./SecuritySettings";
import PluginManager from "./PluginManager";
import AboutSettings from "./AboutSettings";
import { FEATURES, type FeatureKey } from "../../lib/features";

/**
 * 设置分类全集。这里是**唯一事实源**：导航顺序、搜索、设置体都由这张表出。
 *
 * 原先「导航用的 CATEGORIES 数组」与「渲染用的 Category switch」是两处，加分类漏写
 * case 时既没 default 也没返回类型标注，tsc 不报错，表现是点进去一片空白。
 * 现在 `Record<CategoryId, CategoryDef>` 强制每个 id 都必须带设置体，漏写在编译期即失败。
 *
 * `lib/commands.ts` 的 `SettingsCat` 是本联合的子集（只给部分分类开了命令入口）。
 */
export type CategoryId =
  | "general"
  | "ai"
  | "memory"
  | "terminal"
  | "appearance"
  | "security"
  | "plugin"
  | "about";

/** 设置体的渲染入参：`onClose`/`info` 只有部分实现用到，按需取。 */
type BodyProps = { onClose: () => void; info: AppInfo | null };

type CategoryDef = {
  /** 搜索兜底用的中文原文；分类名的界面文案走 i18n `cat.<id>`。 */
  label: string;
  /** 搜索关键词（中英双语并存）。该分类内容增减时记得回来补，否则搜索失配。 */
  keys: string;
  Body: (props: BodyProps) => ReactNode;
};

/** 表内书写顺序即左侧导航顺序。无图标、少而清。 */
const CATEGORIES: Record<CategoryId, CategoryDef> = {
  general: {
    label: "通用",
    keys: "语言 language 界面 ui 通用 general 国际化 i18n 通知 notification 横幅 banner os",
    Body: () => <GeneralSettings />,
  },
  ai: {
    label: "AI",
    keys: "ai 模型 provider byok ollama claude openai deepseek 温度 tokens 脱敏 工具 网页 往返 rounds 步数 上限",
    /** Provider 配置复用既有管理器（嵌入态）；提示词模板组件内部按 devMode 自门禁，关则返回 null。 */
    Body: ({ onClose }) => (
      <>
        <ProviderSettings onClose={onClose} embedded />
        <AiPrefs />
        <PromptTemplatePanel />
      </>
    ),
  },
  memory: {
    label: "记忆",
    keys: "memory 记忆 提取 保留 retention 关键词 自动",
    Body: ({ onClose }) => <MemoryManager onClose={onClose} embedded />,
  },
  terminal: {
    label: "终端",
    keys: "terminal shell 字号 字体 光标 排版 行高 字距 滚动 scrollback 触发器 trigger 本地 shell",
    Body: ({ onClose }) => (
      <>
        <TerminalFont />
        <TerminalPrefs />
        <TriggerManager onClose={onClose} embedded />
      </>
    ),
  },
  appearance: {
    label: "外观",
    keys: "theme 主题 颜色 亮色 暗色 json",
    Body: ({ onClose }) => <AppearanceSettings onClose={onClose} embedded />,
  },
  security: {
    label: "安全",
    keys: "安全 审批 出网 脱敏 策略 审计 privacy 指纹 fingerprint proxyjump 跳板 锁屏 lock pin 空闲 idle",
    Body: ({ onClose }) => <SecuritySettings onClose={onClose} embedded />,
  },
  plugin: {
    label: "插件",
    keys: "plugin 插件 wasm manifest 权限",
    Body: ({ onClose }) => <PluginManager onClose={onClose} embedded />,
  },
  about: {
    label: "关于",
    keys: "关于 about 更新 update 版本 version 协议 能力 边界 限制 transfer release",
    Body: ({ info }) => <AboutSettings info={info} />,
  },
};

/** 各功能开关控制的设置分类：开关关时，对应分类从设置中心隐藏（开关开启时自动恢复）。 */
const FEATURE_CATS: Partial<Record<FeatureKey, CategoryId[]>> = {
  plugins: ["plugin"],
};
const HIDDEN_CATS = new Set<CategoryId>(
  (Object.keys(FEATURE_CATS) as FeatureKey[]).flatMap((k) => (FEATURES[k] ? [] : FEATURE_CATS[k] ?? [])),
);
/** 当前对用户可见的分类 id（已剔除未开放功能）；Object.keys 保持表内书写顺序。 */
const BASE_CATEGORIES = (Object.keys(CATEGORIES) as CategoryId[]).filter((id) => !HIDDEN_CATS.has(id));

interface Props {
  onClose: () => void;
  /** 打开时定位到的分类（缺省第一个「通用」）。 */
  initial?: CategoryId;
}

/**
 * 设置中心：左侧分类 + 搜索，右侧设置体。分类按「用得着」合并，不堆图标。
 * 安全红线：只读写非敏感偏好。
 */
export default function SettingsCenter({ onClose, initial = "general" }: Props) {
  const [cat, setCat] = useState<CategoryId>(initial);
  const [query, setQuery] = useState("");
  const [info, setInfo] = useState<AppInfo | null>(null);
  const t = useI18n();

  useEffect(() => {
    void settingsActions.refresh();
    void appearanceActions.refresh();
    void terminalActions.refresh();
    void providerActions.refresh();
    void memoryActions.refresh();
    void snippetActions.refresh();
    void sshKeyActions.refresh();
    void triggerActions.refresh();
    void tunnelActions.refresh();
    appApi
      .info()
      .then(setInfo)
      .catch(() => {});
  }, []);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return BASE_CATEGORIES;
    return BASE_CATEGORIES.filter((id) => {
      const c = CATEGORIES[id];
      return (
        c.label.toLowerCase().includes(q) ||
        t(`cat.${id}`).toLowerCase().includes(q) ||
        q.split(/\s+/).some((w) => c.keys.includes(w))
      );
    });
  }, [query, t]);

  useEffect(() => {
    if (visible.length > 0 && !visible.includes(cat)) setCat(visible[0]);
  }, [visible, cat]);

  const { Body } = CATEGORIES[cat];

  // 设置中心此前既无 Esc、遮罩又是 `onClick`（在页内拖选文字松手在遮罩上就整窗误关）。
  useEscClose(onClose);

  return (
    <div className="modal-mask" {...useMaskDismiss(onClose)}>
      <div className="modal sc" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2>{t("settings.title")}</h2>
          <input
            className="sc-search"
            placeholder={t("settings.search")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            spellCheck={false}
          />
          <button className="modal-x" onClick={onClose} aria-label={t("common.close")}>
            ×
          </button>
        </div>

        <div className="sc-body">
          <nav className="sc-nav">
            {visible.map((id) => (
              <button
                key={id}
                type="button"
                className="sc-cat"
                data-on={cat === id}
                onClick={() => setCat(id)}
              >
                {t(`cat.${id}`)}
              </button>
            ))}
            {visible.length === 0 && <div className="sc-empty">{t("settings.noMatch")}</div>}
          </nav>

          <div className="sc-pane">
            <div className="sc-title">{t(`cat.${cat}`)}</div>
            <Body onClose={onClose} info={info} />
          </div>
        </div>
      </div>
    </div>
  );
}
