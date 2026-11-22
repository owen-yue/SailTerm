// addon-search 0.16 的行为约束：终端搜索的计数/导航语义全建立在这几条上，改动前先跑这里。
// jsdom 里不调 open()，选区服务不存在，故把 addon 用到的 select/getSelectionPosition/clearSelection
// 用假实现替掉（语义与真实一致：记位、给位、清位），resultCount 走 tracker，与渲染器无关。
// 命中标记的取色数学不在这里，在 themes.test.ts。
import { describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
import { SearchAddon } from "@xterm/addon-search";

const deco = {
  decorations: {
    matchBackground: "#ffcc00",
    matchOverviewRuler: "#ffcc00",
    activeMatchBackground: "#00ccff",
    activeMatchColorOverviewRuler: "#00ccff",
  },
};

function countIn(term: Terminal, needle: string): number {
  let total = 0;
  for (let i = 0; i < term.buffer.active.length; i++) {
    const s = term.buffer.active.getLine(i)?.translateToString(true) ?? "";
    total += s.toLowerCase().split(needle.toLowerCase()).length - 1;
  }
  return total;
}

type Spot = { col: number; row: number; size: number };

async function build(lines: string[], cols = 80) {
  const seen: Spot[] = [];
  const term = new Terminal({
    allowProposedApi: true,
    cols,
    rows: 24,
    scrollback: 1000,
    convertEol: true,
  });
  // jsdom has no renderer/selection service (open() is never called): stub the
  // selection surface the addon relies on so find* + result counting can run.
  let start: { x: number; y: number } | undefined;
  let end: { x: number; y: number } | undefined;
  const t = term as unknown as Record<string, unknown>;
  t.select = (col: number, row: number, size: number) => {
    seen.push({ col, row, size });
    start = { x: col, y: row };
    end = { x: col + size, y: row }; // 真实 xterm 的 end 不含末格（写成 inclusive 会让同词再搜原地踏步）
  };
  t.getSelectionPosition = () => (start && end ? { start, end } : undefined);
  t.clearSelection = () => {
    start = undefined;
    end = undefined;
  };

  const search = new SearchAddon();
  term.loadAddon(search);
  const events: unknown[] = [];
  const api = search as unknown as { onDidChangeResults: (cb: (e: unknown) => void) => void };
  api.onDidChangeResults((e) => events.push(e));

  await new Promise<void>((res) => term.write(lines.join("\r\n") + "\r\n", () => res()));
  const last = () => JSON.stringify(events[events.length - 1]);
  return { term, search, seen, last };
}

describe("search probe", () => {
  it("short lines", async () => {
    const { term, search, last } = await build([
      "[root@host ~]# ls1s1s",
      "-bash: ls1s1s: command not found",
      "[root@host ~]#",
    ]);
    const found = search.findNext("ls", deco);
    expect(found).toBe(true);
    expect(JSON.parse(last()).resultCount).toBe(countIn(term, "ls"));
  });

  it("wrapped long line", async () => {
    const long = "echo " + "aaa ".repeat(30) + "ls " + "bbb ".repeat(30) + "ls " + "ccc ".repeat(30) + "ls";
    const { term, search, last } = await build([long, "tail ls here"], 80);
    search.findNext("ls", deco);
    expect(JSON.parse(last()).resultCount).toBe(countIn(term, "ls"));
  });

  it("matches before and after a wrapped line", async () => {
    const long = "echo " + "xxx ".repeat(40) + "ls";
    const { term, search, last } = await build(["first ls here", long, "last ls here"], 80);
    search.findNext("ls", deco);
    expect(JSON.parse(last()).resultCount).toBe(countIn(term, "ls"));
  });

  it("typing l then ls", async () => {
    const { term, search, last } = await build(["ls one", "two ls", "three ls ls"]);
    search.findNext("l", deco);
    search.findNext("ls", deco);
    expect(JSON.parse(last()).resultCount).toBe(countIn(term, "ls"));
  });

  it("typing keeps advancing unless the query change resets the selection", async () => {
    const { term, search, seen } = await build(["ls one", "two ls", "three ls ls"]);
    // 真实 UI 序列：搜 “ls” → Enter 推进到第二个 → 删一个字符变回 “l”。addon 以当前选区为起点，
    // 改词后从“第二个命中那一行”往后找，跳过了 row 0 就存在的第一个 “l”——这就是“当前
    // 命中的那个”说不清在哪的根因，也是 runSearch 必须在词变时 clearSelection 的理由。
    search.findNext("ls", deco); // row 0
    search.findNext("ls", deco); // Enter → row 1
    search.findNext("l", deco); // 改词：仍从 row 1 的选区起算
    expect(seen[seen.length - 1].row).toBe(1);
    search.clearDecorations();
    (term as unknown as { clearSelection: () => void }).clearSelection();
    search.findNext("l", deco);
    expect(seen[seen.length - 1].row).toBe(0); // 清完选区 = 从缓冲区开头重搜，恒定命中第一个
  });

  it("geometry of successive findNext", async () => {
    const { search, seen } = await build(["[root@host ~]# ls1s1s", "-bash: ls1s1s: not found"]);
    for (let i = 0; i < 5; i++) {
      const ok = search.findNext("ls", deco);
      expect(ok).toBe(true);
      expect(seen[seen.length - 1].size).toBe(2); // 标记宽度=词长，不会多盖格子
    }
  });
});
