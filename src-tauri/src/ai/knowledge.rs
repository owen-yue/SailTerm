//! 内置知识库（AI 技能之「knowledge」源）：把 SailTerm 自己的帮助语料变成模型可查的能力。
//!
//! 语料与前端帮助页**同源不同读**：同一份 `src/content/faq.<lang>.md`，前端经 Vite `?raw`
//! 渲染给人看（[`lib/faq.ts`]），后端经 `include_str!` 编译期内嵌供 AI 检索（同款先例：db
//! 迁移脚本）。单一真相源——加一条语料，人和 AI 同时获得，版本天然同步、离线可用。
//!
//! 解析规则与 `lib/faq.ts` 逐字对齐：`## 标题` 起一条，`#` 主标题与前言不入，正文聚到
//! 下一个 `## `（或 `#` / 文件末尾）为止。改动 md 格式约定时两处必须同步。
//!
//! 检索**故意不做重**（不上 FTS5 / 向量）：语料是人工维护的短条目（几十条量级），
//! 启动期一次解析进内存、线性打分即可；召回质量不够时再升级，接口不变。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// 中文语料：与前端 `lib/faq.ts` 指向同一文件（路径以本文件为基准：三级回仓库根）。
/// pub+hidden 仅供排序探针（独立零依赖包）原样编入本模块时引用，不进任何对外 API。
#[doc(hidden)]
pub const MD_ZH: &str = include_str!("../../../src/content/faq.zh.md");
/// 英文语料。
#[doc(hidden)]
pub const MD_EN: &str = include_str!("../../../src/content/faq.en.md");

/// 一条答疑：问题与答案均为语料里的字面文案（非 i18n 键，语言由所在语料决定）。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub q: String,
    pub a: String,
}

/// 一次检索命中：条目 + 来源语料 + 得分（降序返回，供上层展示与调试）。
#[derive(Debug)]
pub struct Hit<'a> {
    pub entry: &'a Entry,
    /// 来源语料码（"zh" / "en"）。
    pub lang: &'static str,
    pub score: f32,
}

/// 收录下限：低于此分的只是「碰巧共现一个词」的噪声（如任意查询都命中含 "SailTerm" 的条目），
/// 交给工具层回「无相关条目」比端出弱相关更诚实。
const MIN_SCORE: f32 = 0.5;

/// 按 `lib/faq.ts` 的约定解析语料（见模块头）。`##` 起条、`#` 收尾且不入库。
/// 标题里的稳定编号前缀 `[N]` 剥掉不入题（前端 faq.ts 同样剥进 `no` 字段，
/// 供直达定位用；AI 检索不需要，留着反而钉死标题文案）。
pub fn parse(md: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    let flush = |cur: &mut Option<(String, Vec<String>)>, out: &mut Vec<Entry>| {
        if let Some((q, body)) = cur.take() {
            out.push(Entry {
                q,
                a: body.join("\n").trim().to_string(),
            });
        }
    };
    for raw in md.lines() {
        let line = raw.trim_end();
        // 裸 `##`（退化空标题）也起条：收尾当前条，自身随后被空标题过滤掉，
        // 不能吞进上一条正文（与 faq.ts 同口径）。
        if line.starts_with("## ") || line.starts_with("##\t") || line == "##" {
            flush(&mut cur, &mut out);
            let mut title = line[2..].trim();
            // 剥稳定编号前缀 `[N]`（仅纯数字）；非编号的方括号开头不动。
            if let Some(rest) = title.strip_prefix('[') {
                if let Some(end) = rest.find(']') {
                    let num = &rest[..end];
                    if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                        title = rest[end + 1..].trim();
                    }
                }
            }
            cur = Some((title.to_string(), Vec::new()));
        } else if line.starts_with("# ") {
            flush(&mut cur, &mut out); // 主标题：收尾当前条，本身不进入内容
        } else if let Some((_, body)) = cur.as_mut() {
            body.push(line.to_string());
        }
    }
    flush(&mut cur, &mut out);
    // 与前端口径一致：空正文条目不入检索（语料占位行「暂无内容」不带 ##，本就落不进来）。
    // 标题也不能空：否则命中标题后回给模型一个空答案。
    out.into_iter()
        .filter(|e| !e.q.is_empty() && !e.a.is_empty())
        .collect()
}

fn corpus_zh() -> &'static [Entry] {
    static V: OnceLock<Vec<Entry>> = OnceLock::new();
    V.get_or_init(|| parse(MD_ZH))
}

fn corpus_en() -> &'static [Entry] {
    static V: OnceLock<Vec<Entry>> = OnceLock::new();
    V.get_or_init(|| parse(MD_EN))
}

/// bigram 的 IDF 表（跨双语全库一次性统计）：「什么」「怎么」「为什」这类 FAQ 虚词
/// 几乎条条都含，不分降就会把任意中文查询抬成满分（探针实测）；降权后只有主题词
/// （如「传输记录」「出网」）能撑高得分。键 → 权重 ln(文档数/含该字组的文档数)。
fn idf_table() -> &'static HashMap<String, f32> {
    static V: OnceLock<HashMap<String, f32>> = OnceLock::new();
    V.get_or_init(|| {
        let docs: Vec<String> = corpus_zh()
            .iter()
            .chain(corpus_en())
            .map(|e| format!("{}\n{}", e.q, e.a).to_lowercase())
            .collect();
        let n = docs.len() as f32;
        let mut df: HashMap<String, usize> = HashMap::new();
        for d in &docs {
            // 每篇文档的 distinct bigram 计入 DF（重复不算多次）
            let (_, gs) = tokenize(d);
            for g in gs {
                *df.entry(g).or_default() += 1;
            }
        }
        df.into_iter()
            .map(|(g, k)| (g, ((n / k as f32).ln()).max(0.05)))
            .collect()
    })
}

/// 跨双语语料检索 top-`k`。同分时中文库优先（加分只对**本身有命中**的条目生效，
/// 零分条目不参与——用户界面语境以中文为主）。空查询/全无关返回空集。
pub fn search(query: &str, k: usize) -> Vec<Hit<'static>> {
    let mut hits: Vec<Hit<'static>> = Vec::new();
    for (lang, entries, bonus) in [("zh", corpus_zh(), 1.0f32), ("en", corpus_en(), 0.0f32)] {
        for e in entries {
            let raw = score_entry(query, &e.q, &e.a);
            let s = if raw > 0.0 { raw + bonus } else { 0.0 };
            if s >= MIN_SCORE {
                hits.push(Hit {
                    entry: e,
                    lang,
                    score: s,
                });
            }
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(k);
    hits
}

/// 单条打分：问题命中权重高于答案命中。0 = 完全无关。
fn score_entry(query: &str, q: &str, a: &str) -> f32 {
    let (qt, qb) = tokenize(query);
    if qt.is_empty() && qb.is_empty() {
        return 0.0;
    }
    let sq = score_text(&qt, &qb, &q.to_lowercase());
    let sa = score_text(&qt, &qb, &a.to_lowercase());
    // 问题里命中说明"问的就是这条"；答案命中只是相关。加权合并。
    sq * 2.0 + sa
}

/// 一段文本对查询的匹配得分（0 = 无关）。两条主路：纯拉丁查询走整词命中（全含满分、
/// ≥60% 且含非泛词按比例给分）；含 CJK 走 bigram IDF 加权覆盖率。
/// 拉丁 token 不做子串部分给分：否则任意含 "sailterm"/"ftp" 的条目都会被非目标查询
/// 沾边抬进 top-3（探针实测的噪声源）。
fn score_text(tokens: &HashSet<String>, bigrams: &HashSet<String>, hay: &str) -> f32 {
    let words: HashSet<&str> = hay
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let word_hit = |t: &String| -> bool {
        // 整词命中，接简单单复数兑：permission 能上 permissions（不做词形分析，够用即止）。
        words.contains(t.as_str())
            || (t.len() > 3 && words.contains(t.trim_end_matches('s')))
            || words.iter().any(|w| w.len() > 3 && *w == t.trim_end_matches('s'))
    };
    // 全含捷径（纯拉丁查询，整词判定）：所有词都出现 = 问的就是这条，直接满分。
    // 仍需至少一个非泛词：否则 “how to change” 这类全虚词查询只要语料里恰好含这几个词
    // 就拿满分（下面主路设同样的门，捷径不能比主路松）。CJK 成分的存在判定交给 bigram 主路。
    if !tokens.is_empty()
        && bigrams.is_empty()
        && tokens.iter().all(|t| word_hit(t))
        && tokens.iter().any(|t| !is_generic_word(t))
    {
        return 3.0;
    }
    let mut s = 0.0f32;
    // 拉丁 token 参评门限：命中率≥⅓ 且至少命中一个非泛词，否则整份贡献记 0
    // （探针实测：不设门时 “quantum …” 的弱命中叠加语料库加分能虚抬进榜）。
    let latin_valid = !tokens.is_empty() && {
        let hit = tokens.iter().filter(|t| word_hit(t)).count();
        hit * 3 >= tokens.len()
            && tokens.iter().any(|t| word_hit(t) && !is_generic_word(t))
    };
    if !bigrams.is_empty() {
        // CJK：命中项按 IDF 加权——主题词（文档数少）贵、虚词（条条都含）便宜；
        // 除以「查询自身 bigram 的 IDF 总和」归一，长句与短词查询可比。
        let idf = idf_table();
        let w = |g: &String| idf.get(g).copied().unwrap_or(1.0);
        let tot: f32 = bigrams.iter().map(w).sum();
        let hit: f32 = bigrams.iter().filter(|g| hay.contains(g.as_str())).map(w).sum();
        if tot > 0.0 {
            s += 3.0 * (hit / tot).min(1.0);
        }
        // 混排查询（如「AI 出网开关在哪里」）：达门限的拉丁命中补一份轻量加分（最多 1 分）。
        if latin_valid {
            s += tokens.iter().filter(|t| word_hit(t)).count() as f32 / tokens.len() as f32;
        }
    } else if latin_valid {
        // 纯拉丁部分命中：按命中率给分（最高 3 分）。
        s += 3.0 * tokens.iter().filter(|t| word_hit(t)).count() as f32 / tokens.len() as f32;
    }
    s
}

/// 英文泛词：单独命中不构成相关（查询里几乎必然出现，不具区分度）。
fn is_generic_word(t: &str) -> bool {
    matches!(
        t,
        "the" | "a" | "an" | "is" | "are" | "to" | "of" | "and" | "or" | "in" | "on"
            | "for" | "how" | "what" | "why" | "can" | "do" | "does" | "i" | "my" | "it"
            | "with" | "where" | "when" | "change"
    )
}

/// 查询切分：拉丁/数字按空白与 CJK 边界切 token；CJK 连续串只产 bigram，**不入 token**——
/// 整串当 token 做全含判定会把「哪里能看到传输记录」这类句子当成单词，整句命中抬 3 分、
/// 单字命中又撑穿收录线，两头都是假信号（探针实测）。
/// bigram 是中文检索的最低成本召回手段：“传输记录”能命中“传输面板里的记录”里的字组。
fn tokenize(q: &str) -> (HashSet<String>, HashSet<String>) {
    let lower = q.to_lowercase();
    let mut tokens: HashSet<String> = HashSet::new();
    let mut bigrams: HashSet<String> = HashSet::new();
    let mut word = String::new(); // 拉丁/数字串（空白与 CJK 截断）
    let mut run: Vec<char> = Vec::new(); // CJK 连续串
    for ch in lower.chars() {
        if is_cjk(ch) {
            if !word.is_empty() {
                tokens.insert(std::mem::take(&mut word));
            }
            run.push(ch);
        } else if ch.is_alphanumeric() {
            flush_run(&mut run, &mut bigrams);
            word.push(ch);
        } else {
            // 标点/空白：两侧串都收尾
            if !word.is_empty() {
                tokens.insert(std::mem::take(&mut word));
            }
            flush_run(&mut run, &mut bigrams);
        }
    }
    if !word.is_empty() {
        tokens.insert(word);
    }
    flush_run(&mut run, &mut bigrams);
    (tokens, bigrams)
}

/// CJK 连续串入库：多字串产全 bigram；单字串作 unigram 兑底（否则“删”这类单字查询永远没 bigram 可查）。
/// 整串**不入 token**：长句 token 做 `contains` 全含判定会制造满权重假信号（探针实测）。
fn flush_run(run: &mut Vec<char>, bigrams: &mut HashSet<String>) {
    if run.len() == 1 {
        bigrams.insert(run[0].to_string());
    }
    for w in run.windows(2) {
        bigrams.insert(w.iter().collect());
    }
    run.clear();
}

fn is_cjk(ch: char) -> bool {
    matches!(ch,
        '\u{4E00}'..='\u{9FFF}'   // 基本汉字
        | '\u{3400}'..='\u{4DBF}' // 扩展 A
        | '\u{F900}'..='\u{FAFF}' // 兼容表意
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# 主标题（不入库）
维护说明前言第一行（不入库）。
前言第二行。

## 传输记录在哪里看？
在底部传输面板。已完成任务保留在列表里，可随时重开。

## 第二个问题
第二条答案。
继续一行答案。

##
";

    #[test]
    fn parses_by_h2_and_skips_preamble() {
        let es = parse(SAMPLE);
        assert_eq!(es.len(), 2);
        assert_eq!(es[0].q, "传输记录在哪里看？");
        assert_eq!(es[0].a, "在底部传输面板。已完成任务保留在列表里，可随时重开。");
        assert_eq!(es[1].q, "第二个问题");
        assert_eq!(es[1].a, "第二条答案。\n继续一行答案。");
    }

    #[test]
    fn shipped_corpora_are_parsed() {
        // 内嵌语料必须可解析（不锁条数——语料随版本增删，锁了就是自我维护负担）。
        let _ = corpus_zh();
        let _ = corpus_en();
    }

    #[test]
    fn cjk_bigram_recalls_paraphrased_question() {
        // 措辞与语料标题不同（倒装+换词），依赖 bigram 重叠命中；钉住内置语料首条标题。
        let hits = search("哪里能看到传输记录", 3);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].entry.q, "在哪能看到传输记录？之前的传输还查得到吗？");
    }

    #[test]
    fn latin_token_matches_english_corpus() {
        let hits = search("file permission change", 3);
        assert!(!hits.is_empty(), "英文查询应能命中英文语料");
    }

    /// 回归钉：全泛词查询不得靠「全含捷径」拿满分——虚词不具区分度，回弱相关比回
    /// 一个凭空满分更诚实。
    #[test]
    fn all_generic_latin_query_does_not_take_shortcut() {
        assert!(search("how to change", 3).is_empty(), "全虚词查询不应上榜");
    }

    #[test]
    fn no_overlap_scores_zero_and_empty_query_is_empty() {
        assert!(search("", 3).is_empty());
        assert!(search("   ", 3).is_empty());
        let hits = search("量子纠缠谱学", 3);
        assert!(hits.is_empty(), "无关查询不应有命中：{hits:?}");
    }

    /// 探针回归钉：**零分条目不得靠语料库加分上榜**（曾把任意英文查询抬进中文库 top-3）。
    #[test]
    fn bonus_only_applies_to_nonzero_entries() {
        let hits = search("quantum entanglement spectroscopy", 3);
        assert!(hits.is_empty(), "全无关应零命中：{hits:?}");
    }

    /// 探针回归钉：FAQ 虚词（什么/怎么）已由 IDF 降权，混排查询里拉丁词整词命中
    /// 才能拉开差距，而非条条抬满分。
    #[test]
    fn mixed_query_ranks_by_topic_bigrams_not_function_words() {
        let hits = search("怎么控制ai出网", 3);
        assert!(!hits.is_empty());
        assert!(hits[0].entry.q.contains("出网"), "首位应是出网条：{:?}", hits[0].entry.q);
        // 得分梯度存在（不是齐刷同分）：首尾分差 > 1 才算 IDF 降权在工作。
        if hits.len() > 1 {
            assert!(hits[0].score - hits.last().unwrap().score > 1.0, "得分无梯度：{hits:?}");
        }
    }

    #[test]
    fn top_k_truncates() {
        let hits = search("传输", 1);
        assert!(hits.len() <= 1);
    }
}
