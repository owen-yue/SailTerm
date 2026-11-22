// 敏感文件识别（lib/sensitive）+ 路径补全纯函数（lib/complete）单测。
// sensitive：只做文件名层面提示、目录不判定；complete：路径 token 拆分与 ghost 延伸。
import { describe, expect, it } from "vitest";
import { isSensitive } from "../sensitive";
import {
  commonExtension,
  isPathToken,
  lastToken,
  lineWithoutLastToken,
  matchNames,
  pathGhostSuffix,
  splitPathToken,
} from "../complete";

describe("isSensitive 凭证文件名识别", () => {
  it("目录一律不判定（目录名撞车太常见）", () => {
    expect(isSensitive("id_rsa", true)).toBe(false);
    expect(isSensitive(".ssh", true)).toBe(false);
  });

  it(".env 家族与密钥扩展名", () => {
    expect(isSensitive(".env")).toBe(true);
    expect(isSensitive(".env.local")).toBe(true);
    expect(isSensitive("server.pem")).toBe(true);
    expect(isSensitive("KEY.P12")).toBe(true); // 大小写不敏感
    expect(isSensitive("nginx.conf")).toBe(false);
  });

  it("精确名与 `name.*` 形式命中，前缀相似不误伤", () => {
    expect(isSensitive("id_rsa")).toBe(true);
    expect(isSensitive("id_rsa.pub")).toBe(true); // startsWith("id_rsa.")
    expect(isSensitive("my.cnf")).toBe(false); // 缺前导点：`.my.cnf` 才是凭证
    expect(isSensitive("readme.txt")).toBe(false);
  });
});

describe("complete 路径补全纯函数", () => {
  it("isPathToken：含 / 或 ~/ ./ ../ 形态", () => {
    expect(isPathToken("a/b")).toBe(true);
    expect(isPathToken("~/.bashrc")).toBe(true);
    expect(isPathToken("./x")).toBe(true);
    expect(isPathToken("grep")).toBe(false);
    // 以 . 开头的 token（如隐藏文件名）按约定算路径形态
    expect(isPathToken(".hidden")).toBe(true);
  });

  it("splitPathToken 拆目录前缀与名称前缀", () => {
    expect(splitPathToken("a/b/c")).toEqual({ dir: "a/b/", prefix: "c" });
    expect(splitPathToken("ab")).toEqual({ dir: "", prefix: "ab" });
    expect(splitPathToken("a/")).toEqual({ dir: "a/", prefix: "" });
  });

  it("lastToken / lineWithoutLastToken 只关心末 token", () => {
    expect(lastToken("cat a/b/c.t")).toBe("a/b/c.t");
    expect(lastToken("ls ")).toBe("");
    // 语义：只削掉末 token 本身，保留其前面的空白/目录前缀（调用方拼接时自行补全）
    expect(lineWithoutLastToken("cat x")).toBe("cat ");
    expect(lineWithoutLastToken("ls ")).toBe("ls ");
  });

  it("matchNames 前缀过滤 + 目录优先 + 字典序稳定", () => {
    const entries = [
      { name: "zeta", isDir: false },
      { name: "alpha", isDir: false },
      { name: "app", isDir: true },
      { name: "other", isDir: true },
    ];
    expect(matchNames("a", entries).map((e) => e.name)).toEqual(["app", "alpha"]);
    expect(matchNames("q", entries)).toEqual([]);
  });

  it("commonExtension 最长公共前缀（只给超出 prefix 的部分）", () => {
    // abc/abd 公共前缀 "ab"，超出 prefix "a" 的部分 = "b"
    expect(commonExtension("a", ["abc", "abd"])).toBe("b");
    expect(commonExtension("ab", ["abc", "xyz"])).toBe("");
    expect(commonExtension("x", [])).toBe("");
  });

  it("pathGhostSuffix：单目录候选补尾斜杠，多候选给公共延伸", () => {
    expect(pathGhostSuffix("doc", [{ name: "docs", isDir: true }])).toBe("s/");
    expect(pathGhostSuffix("doc", [{ name: "docker", isDir: false }])).toBe("ker");
    expect(
      pathGhostSuffix("a", [
        { name: "abc", isDir: false },
        { name: "abd", isDir: false },
      ]),
    ).toBe("b"); // 公共前缀 "ab" 超出 "a" → "b"
    expect(pathGhostSuffix("a", [])).toBe("");
  });
});
