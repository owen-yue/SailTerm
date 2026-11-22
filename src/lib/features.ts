/**
 * 功能开关（feature flags）：集中管理「代码已实现，但暂不面向用户开放」的功能。
 *
 * 用途：某功能入口需要收起时，把对应项置 false（并移除其入口），
 * 需要开放时改回 true 即可，无需在多处调用点反复增删。
 * 这是纯前端展示层开关，不影响后端能力；仅控制「入口是否出现」。
 */
export const FEATURES = {
  /** 插件系统：设置中心入口与顶栏按钮暂关闭。*/
  plugins: false,
  /** 会话录制：顶栏「录制」按钮与 Ctrl+Shift+R 入口暂关闭。*/
  recording: false,
  /** 自动更新：更新能力尚未接入，暂隐藏「关于」页的「软件更新」入口。*/
  autoUpdate: false,
} as const;

export type FeatureKey = keyof typeof FEATURES;

/** 某功能当前是否对用户开放。 */
export const featureOn = (key: FeatureKey): boolean => FEATURES[key];
