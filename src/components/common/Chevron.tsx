/**
 * 边栏收起/展开统一图标：14px 线稿 chevron，与下栏页签图标同一视觉语言。
 *
 * 之前三处收起按钮各用各的字符（« » ▾ ▴），字形粗细与尺寸都不一致；
 * 全部换成这个组件后方向语义保留（指向收起/展开的方向），观感统一。
 */
export type ChevronDir = "left" | "right" | "up" | "down";

const ROTATE: Record<ChevronDir, number> = { left: 0, up: 90, right: 180, down: 270 };

export default function Chevron({ dir }: { dir: ChevronDir }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ transform: `rotate(${ROTATE[dir]}deg)`, flex: "none" }}
      aria-hidden
    >
      <path d="M10 2.5 5 8l5 5.5" />
    </svg>
  );
}
