//  路径补全（纯函数，不碰 IPC）：从命令行末 token 拆出目录与前缀，并在候选名里做匹配。
// 出网为零——候选来自本机/会话的列目录结果（`file_list` / `local_list`）。

/** 路径形态的 token：含 `/`，或以 `~`/`.` 开头（`~/`、`./`、`../`）。 */
export function isPathToken(token: string): boolean {
  return token.includes("/") || token.startsWith("~") || token.startsWith(".");
}

/** 把末 token 拆成「目录前缀（含尾 /）+ 名称前缀」。`a/b/c` → `{ dir: "a/b/", prefix: "c" }`。 */
export function splitPathToken(token: string): { dir: string; prefix: string } {
  const i = token.lastIndexOf("/");
  if (i < 0) return { dir: "", prefix: token };
  return { dir: token.slice(0, i + 1), prefix: token.slice(i + 1) };
}

/** 取命令行最后一个空白分隔 token（路径补全只关心它）。 */
export function lastToken(line: string): string {
  const parts = line.split(/\s+/);
  return parts[parts.length - 1] ?? "";
}

/** 去掉最后一个 token 后的命令行前缀（含末尾空白语义：不含原空白，调用方拼接时补）。 */
export function lineWithoutLastToken(line: string): string {
  const t = lastToken(line);
  return line.slice(0, line.length - t.length);
}

/** 在候选文件名里挑出 `prefix` 前缀匹配项，目录优先、字典序稳定。 */
export function matchNames(
  prefix: string,
  entries: { name: string; isDir: boolean }[],
): { name: string; isDir: boolean }[] {
  return entries
    .filter((e) => e.name.startsWith(prefix))
    .sort((a, b) => Number(b.isDir) - Number(a.isDir) || a.name.localeCompare(b.name));
}

/** 多个候选的最长公共前缀延伸（超出 `prefix` 的部分）；不足则空串。 */
export function commonExtension(prefix: string, names: string[]): string {
  if (names.length === 0) return "";
  let common = names[0];
  for (const n of names.slice(1)) {
    let i = 0;
    while (i < common.length && i < n.length && common[i] === n[i]) i += 1;
    common = common.slice(0, i);
  }
  return common.length > prefix.length ? common.slice(prefix.length) : "";
}

/** 目录候选的 ghost 后缀：单候选给完整剩余；多候选给公共延伸（可再 Tab 轮换）。 */
export function pathGhostSuffix(
  prefix: string,
  matches: { name: string; isDir: boolean }[],
): string {
  if (matches.length === 0) return "";
  if (matches.length === 1) {
    const m = matches[0];
    return m.name.slice(prefix.length) + (m.isDir ? "/" : "");
  }
  return commonExtension(prefix, matches.map((m) => m.name));
}
