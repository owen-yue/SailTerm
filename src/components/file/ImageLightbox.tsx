import { useEffect, useSyncExternalStore } from "react";
import { useI18n } from "../../lib/i18n";

/**
 * 全屏图片查看层：聊天里的缩略图太小看不清，点开看原图。
 *
 * 为什么不复用既有弹层：`Sheet` 是侧滑面板（承载设置/表单）、`ConfirmDialog`/`FormDialog`
 * 自带按钮与表单焦点管理——一张图用不上那些，硬套只会多出一层无意义的 DOM 与快捷键。
 *
 * 状态放模块级而不是 props 往上抛：缩略图散在时间线各处（抽屉与终端就地面板共用同一渲染
 * 路径），逐层传回调会把中间几层组件的签名都污染一遍。模块级单例 + 全局挂载一次（App.tsx）
 * 与项目里 `toast.show(...)` 的用法同构，调用方一行 `showImage(src, name)` 即可。
 * 只存一张：放大是瞬态视图，不做队列/翻页——多图翻页属另一个需求，别顺手预埋。
 */
type Shot = { src: string; name: string } | null;

let shot: Shot = null;
const subs = new Set<() => void>();

/** 打开大图：`src` 直接复用缩略图已持有的 dataUrl（同一份 base64，不再走一趟 IPC）。 */
export function showImage(src: string, name = ""): void {
  shot = { src, name };
  subs.forEach((f) => f());
}

/** 关闭大图（点遮罩、点图、Esc 三条路都汇到这里）。 */
export function closeImage(): void {
  if (!shot) return;
  shot = null;
  subs.forEach((f) => f());
}

export default function ImageLightbox() {
  const t = useI18n();
  const cur = useSyncExternalStore(
    (cb) => {
      subs.add(cb);
      return () => {
        subs.delete(cb);
      };
    },
    () => shot
  );

  // Esc 只关本层：捕获阶段 + stopPropagation，抢在抽屉/终端的 Esc 处理之前
  // （与 ShortcutHelp 同一套做法，否则关图的同时把 AI 抽屉也收了）。
  useEffect(() => {
    if (!cur) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      closeImage();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [cur]);

  if (!cur) return null;
  return (
    <div
      className="img-lightbox"
      role="dialog"
      aria-label={cur.name || t("common.close")}
      title={t("common.close")}
      onClick={closeImage}
    >
      {cur.name && <span className="img-lightbox-name">{cur.name}</span>}
      <img className="img-lightbox-img" src={cur.src} alt={cur.name} />
    </div>
  );
}
