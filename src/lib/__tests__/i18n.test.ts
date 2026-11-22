// 轻量 i18n（lib/i18n）单测：语料包注册制的核心语义。
// 重点：缺键回退链（当前语言 → zh → 键名本身，绝不空白）、"system" 偏好解析、
// 插件语料注册保留字校验，以及**中英词典键集一致性**（缺键会静默回落中文，
// 是切英文后界面中英混杂的头号来源）。
import { afterAll, describe, expect, it } from "vitest";
import {
  availableLocales,
  extendLocale,
  getUiLangName,
  registerLocale,
  resolveLang,
  setUiLanguage,
  t,
  tReason,
} from "../i18n";
import { enLocale } from "../../locales/en";
import { zhLocale } from "../../locales/zh";

describe("语料包注册表", () => {
  it("内置 zh/en 启动即注册，注册顺序 = 展示顺序", () => {
    const codes = availableLocales().map((p) => p.code);
    expect(codes.slice(0, 2)).toEqual(["zh", "en"]);
  });

  it("`system` 是保留偏好值，空 code 不允许注册", () => {
    registerLocale({ code: "system", label: "x", dict: { a: "b" } });
    registerLocale({ code: "  ", label: "x", dict: { a: "b" } });
    expect(availableLocales().some((p) => p.code === "system")).toBe(false);
  });

  it("extendLocale 只增量合并已注册语言，不凭空造半截语言", () => {
    extendLocale("xx", { any: "x" }); // 未注册：静默跳过
    expect(availableLocales().some((p) => p.code === "xx")).toBe(false);
    const before = t("settings.title");
    extendLocale("zh", { "test.only": "只有中文" });
    expect(t("test.only")).toBe("只有中文");
    expect(t("settings.title")).toBe(before); // 既有键不被覆盖丢失
  });
});

describe("resolveLang（偏好值 → 实际语言）", () => {
  it("已注册 code 直用（大小写/空格不敏感）", () => {
    expect(resolveLang("EN")).toBe("en");
    expect(resolveLang(" en ")).toBe("en");
  });

  it("未注册且系统语言也未装 → 回落兜底 zh", () => {
    const proto = Object.getPrototypeOf(navigator) as object;
    const orig = Object.getOwnPropertyDescriptor(proto, "language");
    Object.defineProperty(proto, "language", { value: "de-DE", configurable: true });
    try {
      expect(resolveLang("fr")).toBe("zh"); // fr 未装 + de 未装 → FALLBACK
    } finally {
      if (orig) Object.defineProperty(proto, "language", orig);
    }
  });

  it("`system` 按 navigator.language 匹配主子标签", () => {
    const proto = Object.getPrototypeOf(navigator) as object;
    const orig = Object.getOwnPropertyDescriptor(proto, "language");
    Object.defineProperty(proto, "language", { value: "en-US", configurable: true });
    try {
      expect(resolveLang("system")).toBe("en"); // en-US → 主子标签 en
    } finally {
      if (orig) Object.defineProperty(proto, "language", orig);
    }
    // 还原后仍确定性可解析：结果必落在已注册语言里
    expect(availableLocales().map((p) => p.code)).toContain(resolveLang("system"));
  });
});

describe("t / setUiLanguage / tReason", () => {
  afterAll(() => setUiLanguage("zh")); // 复位，避免影响同进程其他用例

  it("默认语言 zh，切 en 后取英文语料", () => {
    expect(t("settings.title")).toBe(zhLocale.dict["settings.title"]);
    setUiLanguage("en");
    expect(t("settings.title")).toBe(enLocale.dict["settings.title"]);
    expect(getUiLangName()).toBe("English");
    setUiLanguage("zh");
    expect(getUiLangName()).toBe("简体中文");
  });

  it("缺键回退链：当前语言缺 → zh → 键名本身", () => {
    setUiLanguage("en");
    // zh 有而 en 缺的键：英文界面回落中文而不是空白
    expect(t("test.only")).toBe("只有中文");
    expect(t("no.such.key.anywhere")).toBe("no.such.key.anywhere");
    setUiLanguage("zh");
  });

  it("tReason：命中模板按 args 回填；词典未命中回落后端中文兜底", () => {
    const key = "pol.dropDb";
    const zhTpl = zhLocale.dict[key];
    expect(zhTpl).toBeTruthy();
    const out = tReason(key, { kw: "DROP" }, "backend-fallback");
    expect(out).toBe(zhTpl.replace("{kw}", "DROP"));
    expect(tReason("no.such.reason", {}, "backend-fallback")).toBe("backend-fallback");
  });
});

describe("中英词典键集一致性", () => {
  it("en 与 zh 键集完全相同（缺键会静默露中文）", () => {
    const zhKeys = new Set(Object.keys(zhLocale.dict));
    const enKeys = new Set(Object.keys(enLocale.dict));
    const missingInEn = [...zhKeys].filter((k) => !enKeys.has(k));
    const missingInZh = [...enKeys].filter((k) => !zhKeys.has(k));
    expect(missingInEn).toEqual([]);
    expect(missingInZh).toEqual([]);
  });

  it("占位符（{n}/{kw} 等）双语必须同名出现", () => {
    // 先剔掉 `{{...}}` 形式的示例文本（如 sn.contentLabel 讲“{{变量}} 占位”，非真插值），
    // 只比对单花括号的真插值占位符。
    const ph = (s: string) =>
      (s.replace(/\{\{[^}]*\}\}/g, "").match(/\{[a-zA-Z_]+\}/g) ?? []).sort().join(",");
    for (const k of Object.keys(zhLocale.dict)) {
      if (k === "test.only") continue; // 上面用例注入的中文单语键
      expect(ph(enLocale.dict[k])).toBe(ph(zhLocale.dict[k]));
    }
  });
});
