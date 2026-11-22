//! OSC 133 shell 集成注入。
//!
//! 登录后向 PTY 写入一段**非破坏性**脚本，让 bash/zsh 在提示符与命令边界打印语义标记：
//! `OSC 133;A`（提示符开始）、`;B`（提示符结束 = 可输入起点）、`;C`（命令开始执行）、
//! `;D;<exit>`（命令结束带退出码）。前端 `registerOscHandler(133)` 据此把 AI 触发从
//! 「非备用屏 + 空行」的保守回退升级为**精确门控**（区分 PS1 主提示 vs PS2 续行 vs 命令执行中）。
//!
//! 安全与兼容取舍：
//! - **附加而非替换**：仅向 `PS1/PS2/PS0/PROMPT_COMMAND`（bash）或 `PROMPT/PS2/precmd/preexec`
//!   （zsh）追加标记，绝不覆盖用户既有提示符（starship/p10k/oh-my-zsh 主题原样保留）。
//! - **自探测 shell**：非 bash/zsh（fish/csh/…）经 `if` 判空后 no-op，不改其环境；这类 shell 登录时
//! 会看到一行无害语法提示，随后退回保守门控（退化策略）。
//! - **幂等**：哨兵变量 `__ST_OSC133` 防止重复注入叠加标记。
//! - **不污染历史**：整行前置一个空格，命中 bash `ignorespace` / zsh `HIST_IGNORE_SPACE`（多数发行版默认）。
//! - **可关**：[`ENABLED`] 为一键总开关；按连接档案的细粒度开关待设置 UI 落地后接入
//! （当前无 `settings` 面板，先用全局常量，记为偏差）。
//! - **失败安全**：注入尽力而为——任一环节未生效（主题动态重建提示符吞掉 `B` 标记等），前端因
//!   「从未见过 `B`」自动退回保守门控，绝不会把 AI 触发锁死。

/// 一键总开关。
/// **默认关闭**：OSC133 注入会在提示符上留下残留/多条 `root@…#`（体验差），
/// AI 触发退回保守门控（非备用屏 + 空行），不影响使用。等提示符零污染方案稳定后再打开。
pub const ENABLED: bool = false;

/// 注入 PTY 的脚本（单行；末尾换行由 [`snippet`] 补上以触发执行）。
///
/// 结构：`前置空格 + 哨兵守卫 → 预生成 A/B/C 标记字节 → bash 分支 | zsh 分支`。
/// 每个标记都用 `printf '\033]133;X\007'` 生成后存入变量，再**附加**进对应提示符变量。
const SNIPPET: &str = r##" if [ -z "$__ST_OSC133" ]; then __ST_OSC133=1; __ST_A=$(printf '\033]133;A\007'); __ST_B=$(printf '\033]133;B\007'); __ST_C=$(printf '\033]133;C\007'); if [ -n "$BASH_VERSION" ]; then __st_d(){ local e=$?; printf '\033]133;D;%s\007' "$e"; }; PS1="\[${__ST_A}\]${PS1}\[${__ST_B}\]"; PS2="\[${__ST_C}\]${PS2}"; PS0="${__ST_C}${PS0-}"; case "${PROMPT_COMMAND-}" in *__st_d*) ;; *) PROMPT_COMMAND="__st_d${PROMPT_COMMAND:+; $PROMPT_COMMAND}";; esac; elif [ -n "$ZSH_VERSION" ]; then __st_precmd(){ local e=$?; printf '\033]133;D;%s\007' "$e"; printf '\033]133;A\007'; }; __st_preexec(){ printf '\033]133;C\007'; }; autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook precmd __st_precmd && add-zsh-hook preexec __st_preexec; PROMPT="${PROMPT}%{${__ST_B}%}"; PS2="%{${__ST_C}%}${PS2}"; fi; fi"##;

/// 单行注入：`stty -echo; <script>; stty echo`，一次执行只出一条提示符。
pub fn one_liner() -> String {
    format!(" stty -echo; {}; stty echo\n", SNIPPET.trim_start())
}

/// 返回写入 PTY 的注入脚本（已附末尾换行以触发执行）。
#[allow(dead_code)]
pub fn snippet() -> String {
    format!("{SNIPPET}\n")
}

/// 静默注入序列：先 `stty -echo`（由调用方分段发送并等待生效），再脚本，再 `stty echo`。
/// 回显经前端过滤哨兵与 `stty` 行，用户不应看到整段脚本（UX）。
#[allow(dead_code)]
pub const ECHO_OFF: &[u8] = b"stty -echo\n";
#[allow(dead_code)]
pub const ECHO_ON: &[u8] = b"stty echo\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_is_single_history_safe_line() {
        let s = snippet();
        assert!(s.ends_with('\n'), "须以换行结尾以触发执行");
        assert_eq!(s.matches('\n').count(), 1, "须为单行，避免多次回显");
        assert!(s.starts_with(' '), "须前置空格以命中 ignorespace，不入历史");
    }

    #[test]
    fn emits_all_four_osc133_markers() {
        let s = snippet();
        for m in ["133;A", "133;B", "133;C", "133;D"] {
            assert!(s.contains(m), "缺少 OSC 标记 {m}");
        }
    }

    #[test]
    fn gated_by_shell_family_and_sentinel() {
        let s = snippet();
        assert!(
            s.contains("$BASH_VERSION") && s.contains("$ZSH_VERSION"),
            "须自探测 bash/zsh，其余 shell no-op"
        );
        assert!(s.contains("__ST_OSC133"), "须有幂等哨兵防重复注入");
    }

    #[test]
    fn appends_rather_than_replaces_prompt() {
        let s = snippet();
        assert!(s.contains("${PS1}"), "bash 须保留既有 PS1（附加标记）");
        assert!(s.contains("${PROMPT}"), "zsh 须保留既有 PROMPT（附加标记）");
        assert!(
            s.contains("PROMPT_COMMAND:+"),
            "须链式追加 PROMPT_COMMAND，不覆盖用户既有钩子"
        );
    }

    #[test]
    fn echo_guards_present() {
        assert_eq!(ECHO_OFF, b"stty -echo\n");
        assert_eq!(ECHO_ON, b"stty echo\n");
    }
}
