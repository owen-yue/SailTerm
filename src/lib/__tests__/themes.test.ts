// themes.ts 的搜索命中染色回归。单独成文件：这里只验证纯函数，
// 不需要 searchAddon.test.ts 里那套 xterm 选区表面桩。
import { describe, expect, it } from "vitest";
import { SEARCH_HIT, THEME_PRESETS, contrastRatio, inkFor, tintFor } from "../themes";

/* 把「颜色怪不怪」翻译成可断言的数字。每条线的来历都不是审美偏好，而是踩过一次才写死的：
   • hard 档可以很亮，因为字色会同时换成墨色（theme.selectionForeground → CellColorResolver L124）；
   • soft 档改不了字色（addon 不递 foregroundColor），只能靠 SEARCH_HIT.soft.keep 的对比度预算自保；
   • soft 对底色的对比度就是「黑底上看不看得见标记」的直接指标：早期按固定 lift 一刀切时它只有
     1.20，在近黑底上等于没标记（实测「黑色背景完全看不清」），所以单独立一条下限。 */
describe("tintFor / inkFor（搜索命中的主题自适应染色）", () => {
  // 相对亮度用「与纯黑的对比度」当单调代理，不必再导一个 lum 函数出去
  const lum = (hex: string) => contrastRatio(hex, "#000000");
  const ch = (hex: string, i: number) => parseInt(hex.slice(1 + i * 2, 3 + i * 2), 16);

  const rows = THEME_PRESETS.map((p) => {
    const bg = p.theme.background ?? "#000";
    const fg = p.theme.foreground ?? "#fff";
    const ac = p.theme.yellow ?? "#ffd666";
    const soft = tintFor(bg, ac, fg, SEARCH_HIT.soft.sat, SEARCH_HIT.soft.lift, SEARCH_HIT.soft.keep);
    const hard = tintFor(bg, ac, fg, SEARCH_HIT.hard.sat, SEARCH_HIT.hard.lift, SEARCH_HIT.hard.keep);
    const ink = inkFor(hard, fg);
    return { id: p.id, bg, fg, soft, hard, ink, dark: lum(bg) < lum(fg) };
  });

  it("当前命中：不论有没有换墨色，块上的文字对比度都≥ 4.5", () => {
    for (const r of rows) {
      const on = r.ink ?? r.fg;
      expect(contrastRatio(r.hard, on), `${r.id} hard=${r.hard} ink=${r.ink}`).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("其它命中：不抢正文（字 ≥ 2.5），低对比主题（solarized-dark 原生 4.7）花不起更多", () => {
    for (const r of rows) {
      expect(contrastRatio(r.soft, r.fg), `${r.id} soft=${r.soft}`).toBeGreaterThanOrEqual(2.5);
    }
  });

  it("其它命中对底色：暗色主题 ≥ 1.6、亮色主题 ≥ 1.2（黑底“完全看不清”的守卫）", () => {
    for (const r of rows) {
      const vis = contrastRatio(r.soft, r.bg);
      expect(vis, `${r.id} soft=${r.soft}`).toBeGreaterThanOrEqual(r.dark ? 1.6 : 1.2);
    }
  });

  it("两档分得开（≥ 1.2）且同向：暗色主题 hard 更亮、亮色主题 hard 更暗", () => {
    for (const r of rows) {
      expect(contrastRatio(r.soft, r.hard), `${r.id} soft=${r.soft} hard=${r.hard}`).toBeGreaterThanOrEqual(1.2);
      const further = r.dark ? lum(r.hard) > lum(r.soft) : lum(r.hard) < lum(r.soft);
      expect(further, `${r.id} 两档方向反了：soft=${r.soft} hard=${r.hard}`).toBe(true);
    }
  });

  it("标记是暖琥珀而不是黄绿：R 不低于 G（dracula/monokai 柠檬黄的历史回归）", () => {
    for (const r of rows) {
      expect(ch(r.soft, 0), `${r.id} soft=${r.soft}`).toBeGreaterThanOrEqual(ch(r.soft, 1));
      expect(ch(r.hard, 0), `${r.id} hard=${r.hard}`).toBeGreaterThan(ch(r.hard, 2));
    }
  });

  it("keep 预算只在超支时回退：同一条位移，预算越紧块越暗", () => {
    const args = [0.85, 0.34] as const;
    const loud = tintFor("#0a0e13", "#ffd666", "#d7e2ec", args[0], args[1], 0);
    const capped = tintFor("#0a0e13", "#ffd666", "#d7e2ec", args[0], args[1], 0.55);
    expect(lum(capped)).toBeLessThan(lum(loud));
    expect(contrastRatio(capped, "#d7e2ec")).toBeGreaterThan(contrastRatio(loud, "#d7e2ec"));
  });

  it("输出恒为六位 hex（addon 只认 #rrggbb），三位 hex 入参也能进", () => {
    for (const r of rows) {
      expect(r.soft).toMatch(/^#[0-9a-f]{6}$/);
      expect(r.hard).toMatch(/^#[0-9a-f]{6}$/);
    }
    expect(tintFor("#123", "#ffd666", "#eee", 0.5, 0.2, 0.5)).toMatch(/^#[0-9a-f]{6}$/);
  });

  it("inkFor：分得开就不动字色，动不了才挑反差大的那一边", () => {
    expect(inkFor("#0a0e13", "#d7e2ec")).toBeUndefined(); // 亮字深底，无需改字
    const ink = inkFor("#e9b537", "#d7e2ec"); // 亮块压亮字：1.3:1 那个老错
    expect(ink).toBeDefined();
    expect(contrastRatio("#e9b537", ink as string)).toBeGreaterThan(contrastRatio("#e9b537", "#d7e2ec"));
    expect(contrastRatio("#e9b537", ink as string)).toBeGreaterThanOrEqual(4.5);
    expect(inkFor("#f2f0ea", "#0b1020")).toBeUndefined(); // 亮主题上的亮块：不该改字
  });

  it("灰阶强调色不会染成一片灰（退回暖金 hue）", () => {
    const c = tintFor("#ffffff", "#999999", "#24292e", SEARCH_HIT.hard.sat, SEARCH_HIT.hard.lift, 0);
    expect(ch(c, 0)).toBeGreaterThan(ch(c, 2)); // R > B 即带暖色，没退成中性灰
  });
});
