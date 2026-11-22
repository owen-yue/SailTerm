// 工具模块自带文案（tools/locales.ts）单测。
// 主词典的中英键集一致性由 lib/__tests__/i18n.test.ts 锁，但它看不到 extendLocale
// 增量合并进来的工具键——新增工具只写中文忘了英文时，切英文会静默露中文。
// 这里补两个方向：合并包减去内置词典 = 工具自带键集（双语必须等集），
// 以及注册表里每个工具的 name/desc 必须在双语都有实义译文（漏翻译最典型的形态）。
import { describe, expect, it } from "vitest";
import "../locales"; // 副作用：把工具文案合并进内置 zh/en 语料包
import { TOOL_DEFS } from "../registry";
import { availableLocales } from "../../lib/i18n";
import { enLocale } from "../../locales/en";
import { zhLocale } from "../../locales/zh";

/** 工具自带的那部分键：合并后的整包 − 内置词典。 */
function toolKeys(code: string, builtin: Record<string, string>): Set<string> {
  const pack = availableLocales().find((p) => p.code === code);
  expect(pack).toBeTruthy();
  return new Set(Object.keys(pack!.dict).filter((k) => !(k in builtin)));
}

describe("tools 自带文案", () => {
  it("中英键集完全相同（差集为空）", () => {
    const zhKeys = toolKeys("zh", zhLocale.dict);
    const enKeys = toolKeys("en", enLocale.dict);
    expect(zhKeys.size).toBeGreaterThan(0);
    expect([...zhKeys].filter((k) => !enKeys.has(k))).toEqual([]);
    expect([...enKeys].filter((k) => !zhKeys.has(k))).toEqual([]);
  });

  it("占位符（{n}/{rtt} 等）双语必须同名出现", () => {
    const ph = (s: string) =>
      (s.replace(/\{\{[^}]*\}\}/g, "").match(/\{[a-zA-Z_]+\}/g) ?? []).sort().join(",");
    const zhDict = availableLocales().find((p) => p.code === "zh")!.dict;
    const enDict = availableLocales().find((p) => p.code === "en")!.dict;
    for (const k of toolKeys("zh", zhLocale.dict)) expect(ph(enDict[k])).toBe(ph(zhDict[k]));
  });

  it("每个已注册工具的 name/desc 双语都有真实译文", () => {
    const zhDict = availableLocales().find((p) => p.code === "zh")!.dict;
    const enDict = availableLocales().find((p) => p.code === "en")!.dict;
    for (const tool of TOOL_DEFS) {
      for (const key of [tool.name, tool.desc]) {
        // 缺键时 t() 回落键名本身，直接查词典更直白：值必须非空且不等于键名。
        expect(zhDict[key], `${tool.id} 缺中文`).toBeTruthy();
        expect(zhDict[key]).not.toBe(key);
        expect(enDict[key], `${tool.id} 缺英文`).toBeTruthy();
        expect(enDict[key]).not.toBe(key);
      }
    }
  });

  it("发送属工具窗口而非停靠面板（tool.send.* 在工具包内）", () => {
    // 主词典里不该再留 dock.send / panel.send 这类停靠时代的键。
    expect(toolKeys("zh", zhLocale.dict).has("tool.send.name")).toBe(true);
    expect("dock.send.hint" in zhLocale.dict).toBe(false);
    expect("panel.send" in zhLocale.dict).toBe(false);
  });
});
