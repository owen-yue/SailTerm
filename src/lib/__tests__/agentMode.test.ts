// Agent 模式分流（lib/agentMode）单测：
// 覆盖两套分流策略的边界——isShellLike（猜意图）与 looksLikeQuestion/匹配信号
// （拿不准一律当命令）。重点锁定已知误判边界：中文路径误判、`whatsapp` 误拦、
// CLI 长命令被长度兜底误判为问句。
import { describe, expect, it } from "vitest";
import {
  AI_TRIGGER_PREFIXES,
  QuestionSignal,
  isShellLike,
  looksLikeQuestion,
  matchQuestionSignal,
  withTriggerHint,
} from "../agentMode";

describe("isShellLike（auto 档：从文本猜意图）", () => {
  it("空输入与 `!` 前缀强制按 shell 处理", () => {
    expect(isShellLike("")).toBe(true);
    expect(isShellLike("   ")).toBe(true);
    expect(isShellLike("!ls -al")).toBe(true);
  });

  it("常见命令直接判定为 shell", () => {
    for (const c of ["ls", "git status", "systemctl restart nginx", "df -h"]) {
      expect(isShellLike(c)).toBe(true);
    }
  });

  it("疑问信号优先：问号 / 疑问词开头判为自然语言", () => {
    expect(isShellLike("what is 1+1")).toBe(false);
    expect(isShellLike("为什么连不上")).toBe(false);
    expect(isShellLike("ls 的结果是什么?")).toBe(false);
    // 疑问句先于 CLI 选项判定：反问带 -- 也不当命令
    expect(isShellLike("what does --password do")).toBe(false);
  });

  it("中文路径/提交信息不被劫持，中文口语提问仍走 AI（历史坑）", () => {
    expect(isShellLike("cd 工作项目/")).toBe(true);
    expect(isShellLike('git commit -m "修复登录"')).toBe(true);
    expect(isShellLike("看一下nginx日志")).toBe(false);
    expect(isShellLike("git 怎么回滚")).toBe(false);
  });

  it("管道/重定向/CLI 选项是结构级命令信号", () => {
    expect(isShellLike("ps aux | grep java")).toBe(true);
    expect(isShellLike("echo $HOME > ~/out.txt")).toBe(true);
    // 未列白名单的长命令：靠 `--xxx=值` 选项救回，不被长度兜底误判
    expect(isShellLike("mysql_config_editor set --login-path=local --host=localhost")).toBe(true);
  });

  it("纯英文短句按 token/长度兜底", () => {
    expect(isShellLike("mytool run --fast")).toBe(true);
    expect(isShellLike("explain " + "word ".repeat(30))).toBe(false);
  });
});

describe("matchQuestionSignal / looksLikeQuestion（默认是命令）", () => {
  it("行首显式标记逐条命中", () => {
    for (const p of AI_TRIGGER_PREFIXES) {
      expect(matchQuestionSignal(`${p} 怎么查日志`)).toBe(QuestionSignal.TriggerPrefix);
    }
    // `/ask` 带词界：/askme 不算标记（落到后续规则）
    expect(matchQuestionSignal("/ask me")).toBe(QuestionSignal.TriggerPrefix);
  });

  it("句中含问号 / 中文疑问词 / 英文疑问词", () => {
    expect(matchQuestionSignal("这个报错怎么回事？")).toBe(QuestionSignal.QuestionMark);
    expect(matchQuestionSignal("nginx 为什么 502")).toBe(QuestionSignal.CjkQuestionWord);
    expect(matchQuestionSignal("how to tail logs")).toBe(QuestionSignal.EnQuestionWord);
  });

  it("中文口语短句且首词非已知命令 → 兜底当提问", () => {
    expect(matchQuestionSignal("看一下磁盘占用")).toBe(QuestionSignal.CjkColloquial);
    // 首词是命令且无疑问词：不算提问（`cd 工作项目/` 同口径）
    expect(matchQuestionSignal("cd 工作项目/")).toBe(null);
  });

  it("拿不准一律当命令：陌生英文命令永不被劫持", () => {
    for (const c of ["kubectl get pods -A", "journalctl -u nginx -f", "zz-tool --run"]) {
      expect(looksLikeQuestion(c)).toBe(false);
    }
    expect(looksLikeQuestion("")).toBe(false);
  });
});

describe("AI_TRIGGER_HINT 联动", () => {
  it("提示文案 {trigger} 替换为标记表首项", () => {
    expect(withTriggerHint("输入 {trigger} 提问")).toBe(`输入 ${AI_TRIGGER_PREFIXES[0]} 提问`);
  });
});
