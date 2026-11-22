// restartStore：标记「有需重启才生效的更改待应用」，驱动主界面右下角常驻重启徽标。
// 只承载一个布尔标志，不含任何数据/凭证。
// 目前由「下栏默认高度」等重启生效项改动时点亮；数据目录迁移、自动更新有各自的重启流程，不走这里。
import { createStore } from "./store";

export const restartStore = createStore<{ pending: boolean }>({ pending: false });

export const restartActions = {
  /** 某项「重启后生效」的设置被改动：点亮徽标（幂等）。 */
  request(): void {
    if (!restartStore.get().pending) restartStore.set({ pending: true });
  },
  /** 关隐藏徽标：仅收起提示，不撤销已改的设置。 */
  dismiss(): void {
    restartStore.set({ pending: false });
  },
};
