// 预加载失败卡的「编辑主机」出口：App 注册真实的档案编辑动作，深层组件（ConnectProgress）
// 无需层层透传 onEdit 即可触发。与 dockRegistry 同思路——一个模块级单例，注册一次、随处调用。
let editProfile: ((profileId: string) => void) | null = null;

export const connectUi = {
  /** App 挂载时注册：给定 profileId 打开该档案的连接编辑表单。 */
  setEditProfile(fn: (profileId: string) => void): void {
    editProfile = fn;
  },
  /** 调用已注册的编辑入口；未注册（如弹出窗口）时静默无操作。 */
  editProfile(profileId: string): void {
    editProfile?.(profileId);
  },
};
