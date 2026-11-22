//! 附件抽取器：字节进、Markdown 出。每个格式一个纯函数，**不碰磁盘、不碰 IPC**。
//!
//! 为什么统一收敛成 Markdown（IR）：这是行业既定形态——MarkItDown / Docling / Unstructured /
//! PP-StructureV3 全都把「各种格式 → Markdown」当作出口，因为模型读 Markdown 的召回最好、而我们的
//! 渲染层也已经只认这一种。若每种格式各留一套内部表示，投送侧与提示词侧都要跟着分叉。
//!
//! 纪律一：**解码口径与 [`crate::files::ftp`] 的 `decode_text` 保持一致**——严格 UTF-8 优先，
//! 失败才按 GB18030（GBK 超集）兜底。同一份中文日志在文件面板和 AI 上下文里解出不同的字，
//! 是最难查的那种不一致；两处都写清楚是为了让下一次改动时必须同时改。
//!
//! 纪律二：**绝不静默丢内容**。任何截断、降级、有损解码都要落到 `degraded` 标记上，
//! 由调用方写进清单与界面——用户以为 AI 看了全文、其实只看了被截的一段，比直接报错难查得多。

use std::borrow::Cow;

use crate::error::AppError;

/// 纯文本抽取结果：正文 + 是否走过兜底解码 + 是否含不可打印字节。
pub struct Text {
    pub body: String,
    /// 非空即「UTF-8 解不开、按 GB18030 硬解过」，其中可能有少量坏字节。
    pub lossy: bool,
    /// 命中 NUL 等二进制特征：调用方据此判为 Binary，不喂给模型。
    pub looks_binary: bool,
}

/// 解码文本。BOM 一律剥掉（U+FEFF 进 prompt 会变成看不见的怪空格）。
pub fn plain_text(bytes: &[u8]) -> Text {
    // 二进制特征：前 8KB 里出现 NUL。与 git / `file` 的判据同源，不自己发明阈值组合。
    let head = &bytes[..bytes.len().min(8192)];
    let looks_binary = head.contains(&0u8);
    let (body, lossy) = match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        // 非法 UTF-8 才走本地编码：这是兜底，不是猜测——UTF-8 合法就没必要再试别的。
        Err(_) => {
            let (cow, _enc, had_errors) = encoding_rs::GB18030.decode(bytes);
            match cow {
                Cow::Owned(s) => (s, true),
                Cow::Borrowed(s) => (s.to_string(), had_errors),
            }
        }
    };
    Text {
        body: body.strip_prefix('\u{feff}').unwrap_or(&body).to_string(),
        lossy,
        looks_binary,
    }
}

/// 表格里一个单元格的 Markdown 转义：竖线会截断列、换行会截断行，必须处理。
fn cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' => out.push(' '),
            '\r' => {}
            '\t' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out.trim().to_string()
}

/// 转好的表格：正文 + 总行数 + 实际列出的行数（`rows` < `total` 即被截断）。
pub struct Table {
    pub body: String,
    pub total: usize,
    pub rows: usize,
    pub cols: usize,
}

/// CSV/TSV → Markdown 表。
///
/// 用 `csv` crate 而不是 `split(',')`：带引号的字段里可以有逗号、双引号和换行，手搓的分隔
/// 逻辑在真实导出文件上必错（而 CSV 恰恰是运维最常喂进来的表格来源）。
/// `flexible(true)`：列数不齐的脏行按短的算，不让一行坏数据废掉整份文件。
pub fn table(bytes: &[u8], delim: u8, max_rows: usize) -> Result<Table, AppError> {
    let text = plain_text(bytes);
    if text.looks_binary {
        return Err(AppError::Invalid("看起来不是文本文件".into()));
    }
    let mut rd = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .delimiter(delim)
        .from_reader(text.body.as_bytes());
    let headers = rd
        .headers()
        .map_err(|e| AppError::Invalid(format!("表格表头解析失败：{e}")))?
        .iter()
        .map(cell)
        .collect::<Vec<_>>();
    if headers.is_empty() {
        return Err(AppError::Invalid("表格没有表头".into()));
    }
    let cols = headers.len();
    let mut out = String::new();
    out.push_str("| ");
    out.push_str(&headers.join(" | "));
    out.push_str(" |\n|");
    for _ in 0..cols {
        out.push_str(" --- |");
    }
    let mut rows = 0usize;
    let mut total = 0usize;
    for rec in rd.records() {
        // 坏行计数但不中断：一份 10 万行的导出里三行解析失败，不该让整份文件报废。
        total += 1;
        let Ok(rec) = rec else { continue };
        if rows >= max_rows {
            continue;
        }
        let mut line = String::from("| ");
        for i in 0..cols {
            if i > 0 {
                line.push_str(" | ");
            }
            line.push_str(&cell(rec.get(i).unwrap_or("")));
        }
        line.push_str(" |\n");
        out.push_str(&line);
        rows += 1;
    }
    Ok(Table {
        body: out,
        total,
        rows,
        cols,
    })
}

/// 一个工作薄里最多读几个 sheet：十个台账页同时上屏，没一个能读完。
const SHEET_MAX_COUNT: usize = 5;
/// 单格最多取的字符数：一个单格里粘进一整段 JSON 是真实工单里的事。
const CELL_MAX_CHARS: usize = 200;

/// xlsx / xls / ods → Markdown 表（每个 sheet 一张，多 sheet 用标题分隔）。
///
/// 用 `calamine` 而不是自己拆 zip + XML：xlsx 的共享字符串表、样式索引、公式缓存值
/// 三者互相引用，手写解析器在真文件上必错（而 `.xlsx` 恰恰是业务同事发过来的常客）。
/// 格式由 `open_workbook_auto_from_rs` 按内容探，不拿扩展名当凭据（与本模块上游同口径）。
///
/// 大文件约束：原始字节已被 [`super::MAX_FILE_BYTES`] 卡在入站体积上限以内，解压后按
/// [`SHEET_MAX_COUNT`] 个 sheet / `max_rows` 行 / `max_cols` 列逐层卡住。必须说清
/// **剩下的风险**：calamine 会把单个 sheet 全量读进内存（该 crate 的既有形态），
/// 所以一张贴着入站上限的宽表仍可能占上百 MB——为它写个流式解析器不划算，靠入站体积上限拒。
pub fn sheet(bytes: &[u8], max_rows: usize, max_cols: usize) -> Result<Table, AppError> {
    use calamine::Reader;
    let cur = std::io::Cursor::new(bytes.to_vec());
    let mut wb = calamine::open_workbook_auto_from_rs(cur)
        .map_err(|e| AppError::Invalid(format!("工作簿打开失败：{e}")))?;
    let names = wb.sheet_names();
    if names.is_empty() {
        return Err(AppError::Invalid("工作簿里没有工作表".into()));
    }
    let mut out = String::new();
    let mut cols = 0usize;
    let mut total = 0usize;
    let mut rows = 0usize;
    let mut picked = 0usize;
    for (idx, name) in names.iter().enumerate() {
        if idx >= SHEET_MAX_COUNT {
            // 没读的页也要写进正文：只说「共 12 页」而不说只读了 5 页，模型会把缺的当成不存在。
            out.push_str(&format!(
                "\n\n（后续 {} 个工作表未读取：{}）\n",
                names.len() - idx,
                names[idx..].join("、")
            ));
            break;
        }
        let Ok(range) = wb.worksheet_range(name) else {
            out.push_str(&format!("\n\n（工作表 {name} 读取失败，已跳过）\n"));
            continue;
        };
        if range.is_empty() {
            continue;
        }
        picked += 1;
        let width = range.width().min(max_cols).max(1);
        if picked > 1 {
            out.push('\n');
        }
        out.push_str(&format!("## {name}\n\n"));
        cols = cols.max(width);
        let mut iter = range.rows();
        // 第一行当表头（表格里没表头是例外不是常态，与 CSV 同口径）。
        let header = iter
            .next()
            .map(|r| {
                (0..width)
                    .map(|i| cell(&row_text(r.get(i))))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.push_str("| ");
        out.push_str(&header.join(" | "));
        out.push_str(" |\n|");
        for _ in 0..width {
            out.push_str(" --- |");
        }
        out.push('\n');
        for r in iter {
            total += 1;
            if rows >= max_rows {
                continue; // 继续数行以保留真实的 total，只是不再往正文里贴
            }
            let line = (0..width)
                .map(|i| cell(&row_text(r.get(i))))
                .collect::<Vec<_>>()
                .join(" | ");
            out.push_str("| ");
            out.push_str(&line);
            out.push_str(" |\n");
            rows += 1;
        }
    }
    if picked == 0 {
        // 打开成功、但每个 sheet 都是空的（典型：刚点「新建」还没填数的 xlsx）。
        // 这是「文件里没有数据」，不是「解析失败」——两者语义必须分开：报 Err 会被上层
        // 归成 parse-failed，模型据此说成「工具读不了 xlsx」（能力问题），而事实是「这份是空表」
        // （文件问题）。交调用方按空表降级（见 ingest::extract_ir 的 Table 分支）。
        return Ok(Table {
            body: String::new(),
            total: 0,
            rows: 0,
            cols: 0,
        });
    }
    Ok(Table {
        body: out,
        total,
        rows,
        cols,
    })
}

/// 一行里第 `i` 格取文：超 [`CELL_MAX_CHARS`] 就截并留省略号（截了要在表格里看得出来）。
fn row_text(d: Option<&calamine::Data>) -> String {
    let s = d.map(data_text).unwrap_or_default();
    if s.chars().count() <= CELL_MAX_CHARS {
        return s;
    }
    let cut: String = s.chars().take(CELL_MAX_CHARS).collect();
    format!("{cut}…")
}

/// 单元格 → 文本。数字不留 `.0`（`1000.0` 读作金额时是个干扰），日期走 calamine 的默认格式化。
fn data_text(d: &calamine::Data) -> String {
    match d {
        calamine::Data::Empty => String::new(),
        calamine::Data::String(s) => s.clone(),
        calamine::Data::Float(f) => {
            let v = *f;
            if v == v.trunc() && v.abs() < 1e15 {
                format!("{}", v as i64)
            } else {
                v.to_string()
            }
        }
        other => other.to_string(),
    }
}

/// 一页 PDF 最多取多少解压后字节：这是解压炸弹的**逐页硬闸**。
const PDF_PAGE_BYTES: usize = 256 * 1024;
/// 最多读几页：一本报销单集几十页很常见，全取会先撞 [`super::ingest::MAX_IR_CHARS`]。
const PDF_MAX_PAGES: usize = 200;

/// PDF 文本层抽取结果：正文 + 总页数 + 实际读取的页数。
pub struct PdfText {
    pub body: String,
    pub pages: usize,
    pub listed: usize,
}

/// PDF → 纯文本（只取文本层，**不重建排版**）。
///
/// 用 `lopdf` 且故意走 `extract_text_with_limit` 而不是 `extract_text`：后者对页面流
/// 解压不设上限，一个几 KB 的设限流可以炸出上百 MB——我们的输入是用户选中的任意文件，
/// 正好是「不可信源」。crate 已提供 bomb-safe 变体，没必要自己包一层解压监控。
///
/// 必须明说的局限：没文本层的 PDF（扫描版/图片型）会抽出空——调用方据此标
/// `degraded=scanned`；而中文 CID 字体子集化过的 PDF，抽出的可能是字形码而不可读，
/// 故本函数额外报一个「可读字符占比」，由调用方决定要不要标 `garbled`。
pub fn pdf_text(bytes: &[u8]) -> Result<(PdfText, f32), AppError> {
    let doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| AppError::Invalid(format!("PDF 打开失败：{e}")))?;
    let pages = doc.get_pages();
    let nums: Vec<u32> = pages.keys().copied().take(PDF_MAX_PAGES).collect();
    let body = doc
        .extract_text_with_limit(&nums, PDF_PAGE_BYTES)
        .map_err(|e| AppError::Invalid(format!("PDF 文本层抽取失败：{e}")))?;
    // 可读度：字母/数字/汉字及常见标点算可读，其余（控制字符、U+FFFD、私用区）不算。
    let readable = body
        .chars()
        .filter(|c| {
            c.is_alphanumeric()
                || c.is_whitespace()
                || c.is_ascii_punctuation()
                || ('\u{4e00}'..='\u{9fff}').contains(c)
        })
        .count();
    let ratio = if body.is_empty() {
        1.0
    } else {
        readable as f32 / body.chars().count() as f32
    };
    Ok((
        PdfText {
            body,
            pages: pages.len(),
            listed: nums.len(),
        },
        ratio,
    ))
}

/// 单个 OOXML XML 条目最多取的解压后字节：zip 不报可靠的解压上限，故自己 `take()` 夹紧（解压炸弹硬闸）。
const OOXML_ENTRY_BYTES: u64 = 4 * 1024 * 1024;
/// 一份 pptx 最多读几页幻灯片（docx 只有一个正文条目，用不到）。
const OOXML_MAX_ENTRIES: usize = 200;

/// Office 文档子类型：决定读哪个条目、取哪个文字标签。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ooxml {
    Docx,
    Pptx,
}

/// Office Open XML 浅抽取结果：正文 + 实际读取的幻灯片页数 + 总页数（`units` < `total` 即被截）
/// + 是否有正文条目被 [`OOXML_ENTRY_BYTES`] 切过（`truncated_entry`，调用方须落 degraded）。
pub struct OoxmlText {
    pub body: String,
    pub units: usize,
    pub total: usize,
    pub truncated_entry: bool,
}

/// docx / pptx → Markdown 纯文本。**浅抽取**：只取正文文字与段落分行，样式、表格线、
/// 图片、页眉页脚、批注都不重建——深结构重建要专门引擎，成本与收益不匹配，
/// 而模型多半只要「讲了什么」。
///
/// 解包用 `zip`（与 calamine 同一份、同一条 deflate），不手搜 inflate + 中央目录。文字节点是
/// OOXML 里形状固定的一小撮（docx `<w:t>`、pptx `<a:t>`），按段落切、取节点内文、解 5 个标准实体——
/// 这不是「写一个 XML 解析器」，是所有 docx→text 工具都在做的定向抽取。
pub fn ooxml(bytes: &[u8], kind: Ooxml) -> Result<OoxmlText, AppError> {
    use std::io::Read;
    let cur = std::io::Cursor::new(bytes.to_vec());
    let mut zip = zip::ZipArchive::new(cur)
        .map_err(|e| AppError::Invalid(format!("Office 包打开失败：{e}")))?;
    // 选出要读的条目：docx 固定一个正文；pptx 是若干幻灯片页，按页码数值序排（文件名里 slide10 要排在 slide2 之后）。
    let mut picks: Vec<(u64, usize)> = Vec::new();
    for i in 0..zip.len() {
        let name = match zip.by_index(i) {
            Ok(f) => f.name().map(|c| c.to_string()).unwrap_or_default(),
            Err(_) => continue,
        };
        match kind {
            Ooxml::Docx => {
                if name == "word/document.xml" {
                    picks.push((0, i));
                }
            }
            Ooxml::Pptx => {
                if let Some(n) = slide_no(&name) {
                    picks.push((n, i));
                }
            }
        }
    }
    if picks.is_empty() {
        return Err(AppError::Invalid(
            "不是可识别的 Office 文档（缺正文/幻灯片条目）".into(),
        ));
    }
    picks.sort_by_key(|(k, _)| *k);
    let (text_tag, para_tag) = match kind {
        Ooxml::Docx => ("w:t", "w:p"),
        Ooxml::Pptx => ("a:t", "a:p"),
    };
    let total = picks.len();
    let mut out = String::new();
    let mut units = 0usize;
    let mut truncated_entry = false;
    for (n, idx) in picks.into_iter().take(OOXML_MAX_ENTRIES) {
        let xml = {
            let mut f = zip
                .by_index(idx)
                .map_err(|e| AppError::Invalid(format!("读取 Office 条目失败：{e}")))?;
            let mut buf = Vec::new();
            f.by_ref()
                .take(OOXML_ENTRY_BYTES)
                .read_to_end(&mut buf)
                .map_err(|e| AppError::Io(format!("解 Office 正文失败：{e}")))?;
            // 读满上限 = 被切（zip 里解压后尺寸只供参考，以实读字节为准）
            if buf.len() as u64 >= OOXML_ENTRY_BYTES {
                truncated_entry = true;
            }
            String::from_utf8_lossy(&buf).into_owned()
        };
        let body = xml_text(&xml, text_tag, para_tag);
        units += 1;
        if kind == Ooxml::Pptx {
            out.push_str(&format!("## 第 {n} 页\n\n"));
        }
        out.push_str(body.trim_end());
        out.push('\n');
        if kind == Ooxml::Pptx {
            out.push('\n');
        }
    }
    Ok(OoxmlText {
        body: out.trim_end().to_string(),
        units,
        total,
        truncated_entry,
    })
}

/// 从 `ppt/slides/slideN.xml` 取页码；其余（slideLayouts/slideMasters/notesSlides）不算页。
fn slide_no(name: &str) -> Option<u64> {
    let stem = name.strip_prefix("ppt/slides/")?.strip_suffix(".xml")?;
    stem.strip_prefix("slide")?.parse().ok()
}

/// 按段落切（`</w:p>` / `</a:p>`）后逐段取文字节点，非空段用换行拼接。
fn xml_text(xml: &str, text_tag: &str, para_tag: &str) -> String {
    let para_close = format!("</{para_tag}>");
    let mut lines: Vec<String> = Vec::new();
    for chunk in xml.split(para_close.as_str()) {
        let line = xml_runs(chunk, text_tag);
        if !line.trim().is_empty() {
            lines.push(line);
        }
    }
    lines.join("\n")
}

/// 一段里所有 `<text_tag>…</text_tag>` 的内文拼接（解实体）。不递归、不读属性，只拿文字。
fn xml_runs(chunk: &str, text_tag: &str) -> String {
    let open = format!("<{text_tag}");
    let close = format!("</{text_tag}>");
    let mut out = String::new();
    let mut rest = chunk;
    while let Some(sp) = rest.find(open.as_str()) {
        let after = &rest[sp + open.len()..];
        // 精确标签：紧跟 `>` 或空白（属性），否则 <w:tbl>/<w:tc> 这类前缀会误命中。
        let gt = match after.as_bytes().first() {
            Some(b'>') => Some(0usize),
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => after.find('>'),
            _ => None,
        };
        let gt = match gt {
            Some(g) => g,
            None => {
                rest = after;
                continue;
            }
        };
        let content = &after[gt + 1..];
        match content.find(close.as_str()) {
            Some(end) => {
                out.push_str(&decode_entities(&content[..end]));
                rest = &content[end + close.len()..];
            }
            None => break,
        }
    }
    out
}

/// 解 5 个预定义实体 + 数字实体（&#..; / &#x..;）。解不了的原样保留，不报错。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        if b[i] == b'&' {
            if let Some(sc) = s[i..].find(';') {
                let ent = &s[i + 1..i + sc];
                let decoded = match ent {
                    "amp" => Some("&"),
                    "lt" => Some("<"),
                    "gt" => Some(">"),
                    "quot" => Some("\""),
                    "apos" => Some("'"),
                    _ => None,
                }
                .map(str::to_string)
                .or_else(|| {
                    let cp = if let Some(hex) = ent
                        .strip_prefix('#')
                        .and_then(|r| r.strip_prefix(['x', 'X']))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else if let Some(dec) = ent.strip_prefix('#') {
                        dec.parse::<u32>().ok()
                    } else {
                        None
                    };
                    cp.and_then(char::from_u32).map(|c| c.to_string())
                });
                if let Some(d) = decoded {
                    out.push_str(&d);
                    i += sc + 1;
                    continue;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 头尾采样：装不下时取「前 `head` 字 + 后 `tail` 字」，中间显式声明省略了多少。
///
/// 为什么是头尾而不是只取开头：日志类附件的信息密度天然分布两端——开头是启动/上下文，
/// 结尾是崩溃/结果，只截前缀恰好会把最有用的异常栈切掉（`kubectl logs --tail`、`less +END`
/// 都是同一个道理）。这是 [`super::ingest`] 里「摘要档」投送的实现；后续若改用大纲 +
/// `read_attachment` 工具按需取段，本函数可整体替换，届时只保留给没有结构的可读文本兜底。
pub fn head_tail(text: &str, head: usize, tail: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n <= head + tail {
        return text.to_string();
    }
    let mut out: String = chars[..head].iter().collect();
    out.push_str(&format!(
        "\n\n……（中间省略 {} 字，此处为头尾采样）……\n\n",
        n - head - tail
    ));
    out.extend(chars[n - tail..].iter());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_prefers_strict_utf8_then_gb18030() {
        // 与 `files/ftp.rs::decode_text` 同口径：合法 UTF-8 绝不二次解码（否则中文会被曲解）。
        let t = plain_text("正常中文日志\n".as_bytes());
        assert!(!t.lossy && !t.looks_binary);
        assert_eq!(t.body, "正常中文日志\n");

        // 「服务器」的 GBK 编码在 UTF-8 下非法，必须走兜底且解出原字。
        let gbk = encoding_rs::GB18030.encode("服务器超时").0;
        let t = plain_text(&gbk);
        assert!(t.lossy, "非 UTF-8 要标记为兜底解码");
        assert_eq!(t.body, "服务器超时");
    }

    #[test]
    fn bom_is_stripped_and_nul_means_binary() {
        assert_eq!(plain_text("\u{feff}alpha".as_bytes()).body, "alpha");
        assert!(
            plain_text(b"bin\x00ary").looks_binary,
            "含 NUL 即判二进制，不能喂文本"
        );
    }

    #[test]
    fn csv_table_escapes_pipes_and_counts_rows() {
        let t = table(b"name,note\na,b|c\n\"d,e\",f\n".as_slice(), b',', 10).unwrap();
        assert_eq!(t.total, 2);
        assert_eq!(t.rows, 2);
        assert_eq!(t.cols, 2);
        assert!(
            t.body.contains("b\\|c"),
            "竖线必须转义，否则表被截列：{}",
            t.body
        );
        assert!(t.body.starts_with("| name | note |"));
    }

    #[test]
    fn csv_truncates_without_losing_total() {
        // 截断要留下「一共多少行、只列了多少行」，否则模型会以为自己看到了全表。
        let big = "id,v\n".to_string() + &(1..=50).map(|i| format!("{i},x\n")).collect::<String>();
        let t = table(big.as_bytes(), b',', 5).unwrap();
        assert_eq!(t.total, 50);
        assert_eq!(t.rows, 5);
    }

    #[test]
    fn csv_survives_ragged_and_broken_lines() {
        // flexible + 坏行跳过：一行脏数据不该让整份文件报废。
        let t = table("a,b,c\n1,2\n\"3,\"\n".as_bytes(), b',', 10).unwrap();
        assert_eq!(t.cols, 3);
        assert_eq!(t.rows, 2);
    }

    #[test]
    fn tsv_uses_tab_delimiter() {
        let t = table(b"a\tb\n1\t2\n".as_slice(), b'\t', 10).unwrap();
        assert_eq!(t.cols, 2);
        assert!(t.body.contains("| a | b |"));
    }

    #[test]
    fn head_tail_keeps_both_ends_and_announces_the_gap() {
        let s = "H".repeat(100) + &"M".repeat(100) + &"T".repeat(100);
        let out = head_tail(&s, 40, 30);
        assert!(out.starts_with(&"H".repeat(40)));
        assert!(out.ends_with(&"T".repeat(30)), "尾部必须保住：崩溃栈在末尾");
        // 300 字里保住 40 + 30，中间声明省略的必须是 230：报小了就是在骗模型。
        assert!(out.contains("省略 230 字"), "{out}");
        // 正好装得下（n == head + tail）不算「被截」，不该出现省略声明。
        let exact = "H".repeat(40) + &"T".repeat(30);
        assert_eq!(head_tail(&exact, 40, 30), exact);
        assert!(!head_tail(&exact, 40, 30).contains("省略"));
    }

    /// 合成一份最小的 Office 包：只放抽取器真会读的条目。`Stored` 不碰压缩后端，
    /// 但解压路径与真实 Word/PPT 一致（zip crate 对两种存法走同一套读取接口）。
    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opt = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            w.start_file(*name, opt).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn docx_shallow_extract_keeps_paragraphs_and_drops_non_text_xml() {
        let doc = "<?xml version=\"1.0\"?><w:document><w:body\
            ><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space=\"preserve\"> World</w:t></w:r></w:p>\
            <w:p><w:r><w:t>A &amp; B &#26816; &#x67E5;</w:t></w:r></w:p>\
            <w:p><w:pPr><w:rPr></w:rPr></w:pPr></w:p>\
            <w:p><w:r><w:t>Done &bogus;</w:t></w:r></w:p></w:body></w:document>";
        let t = ooxml(&zip_of(&[("word/document.xml", doc)]), Ooxml::Docx).unwrap();
        assert_eq!(t.units, 1);
        assert_eq!(t.total, 1);
        // 同段多个 run 连成一行，不同段分行；空段不占行；实体被解开，解不了的保留原样。
        assert_eq!(t.body, "Hello World\nA & B 检 查\nDone &bogus;");
    }

    #[test]
    fn prefixed_tags_are_not_mistaken_for_text_nodes() {
        // `<w:tbl>` / `<w:tc>` 与 `<w:t>` 同前缀：不卡精确标签就会把表格 XML 当文字吞进来。
        let t = ooxml(
            &zip_of(&[(
                "word/document.xml",
                "<w:p><w:tbl><w:tc><w:t>in table</w:t></w:tc></w:tbl></w:p>",
            )]),
            Ooxml::Docx,
        )
        .unwrap();
        assert_eq!(t.body, "in table");
        assert!(!t.body.contains("w:tbl"), "{}", t.body);
    }

    #[test]
    fn pptx_slides_are_ordered_numerically_and_titled() {
        // 包内顺序是 slide10, slide2：按字符串排会把 10 排在 2 前面，页码必须按数值序。
        let bytes = zip_of(&[
            ("ppt/slides/slide10.xml", "<a:p><a:t>ten</a:t></a:p>"),
            ("ppt/slides/slide2.xml", "<a:p><a:t>two</a:t></a:p>"),
            (
                "ppt/notesSlides/notesSlide2.xml",
                "<a:t>备注不该进正文</a:t>",
            ),
        ]);
        let t = ooxml(&bytes, Ooxml::Pptx).unwrap();
        assert_eq!(t.total, 2, "notesSlides 不算一页");
        assert_eq!(t.units, 2);
        assert!(t.body.contains("## 第 2 页\n\ntwo"), "{}", t.body);
        assert!(t.body.contains("## 第 10 页\n\nten"), "{}", t.body);
        assert!(t.body.find("第 2 页").unwrap() < t.body.find("第 10 页").unwrap());
        assert!(!t.body.contains("备注"), "{}", t.body);
    }

    #[test]
    fn office_package_without_expected_entry_fails_loudly() {
        // 合法 zip 但没有正文条目（其实是 xlsx/压缩包）：报错，不交一份空正文。
        let bytes = zip_of(&[("xl/workbook.xml", "<x/>")]);
        assert!(ooxml(&bytes, Ooxml::Docx).is_err());
        assert!(ooxml(b"PK\x03\x04", Ooxml::Pptx).is_err());
    }

    #[test]
    fn xml_helpers_are_scoped_to_what_they_claim() {
        assert_eq!(slide_no("ppt/slides/slide42.xml"), Some(42));
        assert_eq!(slide_no("ppt/slides/_rels/slide1.xml.rels"), None);
        assert_eq!(slide_no("ppt/slideMasters/slideMaster1.xml"), None);
        assert_eq!(xml_runs("<a:t>ok</a:t>", "a:t"), "ok");
        assert_eq!(
            decode_entities("&amp;&lt;&gt;&quot;&apos;&#65;&#x42;&why;"),
            "&<>\"'AB&why;"
        );
        // 多字节安全：实体之外按字符步前进，不会切进 UTF-8 中间。
        assert_eq!(decode_entities("中文&中"), "中文&中");
    }

    /// 读 `tests/fixtures/` 下的样本。相对路径从 crate 根算，不依赖运行时工作目录。
    fn fixture(name: &str) -> Vec<u8> {
        use base64::Engine;
        let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("缺 fixture {p}：{e}"));
        base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .unwrap_or_else(|e| panic!("fixture {name} 不是合法 base64：{e}"))
    }

    #[test]
    fn fixture_bytes_survive_copy_round_trip() {
        // 与 `commands/ai.rs::probe_image_decodes_as_png` 同口径：样本损坏要在这里先报错，
        // 不能让下面的断言把它解成「抽取器解不出」的假信号。
        let b = fixture("workbook.xlsx.b64");
        assert_eq!(&b[..2], b"PK", "xlsx 是 zip 容器");
        assert_eq!(b.len(), 2056, "样本被改动过就要重新生成并同步这里");
    }

    #[test]
    fn workbook_to_markdown_keeps_titles_numbers_and_skips_empty_sheets() {
        // 真 xlsx（含中文表头 / 内联字符串 / 数字）走 calamine：表名出标题、整数浮点不留 `.0`、
        // 空表跳过不出现在正文里——与 CSV 共用同一个 Table 出口。
        let t = sheet(&fixture("workbook.xlsx.b64"), 200, 20).unwrap();
        assert_eq!((t.cols, t.total, t.rows), (4, 3, 3), "{:?}", t.body);
        assert!(t.body.starts_with("## 机房清单\n\n"), "{}", t.body);
        assert!(
            t.body.contains("| 机柜 | 功率kW | 上架率 | 备注 |"),
            "{}",
            t.body
        );
        assert!(t.body.contains("| A-01 | 1000 | 7 | 0.75 |"), "{}", t.body);
        assert!(!t.body.contains("1000.0"), "整数不该带小数尾巴：{}", t.body);
        assert!(!t.body.contains("空表"), "空工作表不出现在正文里");
    }

    #[test]
    fn workbook_honours_row_and_column_limits() {
        let b = fixture("workbook.xlsx.b64");
        // 行：截了要保住真实 total（上游据此写「共 N 行仅列 M 行」），不能只留长度。
        let t = sheet(&b, 2, 20).unwrap();
        assert_eq!((t.total, t.rows), (3, 2), "继续数行以保留真实 total");
        // 列：宽表只取前若干列，后面的列名根本不该出现。
        let t = sheet(&b, 200, 2).unwrap();
        assert_eq!(t.cols, 2);
        assert!(!t.body.contains("上架率"), "{}", t.body);
        // 表头被列上限切掉时只剩两列，仍要是合法的 Markdown 表。
        assert!(t.body.contains("| 机柜 | 功率kW |"), "{}", t.body);
    }

    #[test]
    fn empty_workbook_returns_ok_table_not_error() {
        // 刚新建、未填数的 xlsx：calamine 能打开但没有数据行。
        // 必须是 Ok(空 Table) 而非 Err——上层才能区分「空表」与「解析失败」。
        let b = fixture("workbook-empty.xlsx.b64");
        assert_eq!(&b[..2], b"PK", "xlsx 是 zip 容器");
        assert_eq!(b.len(), 4796, "样本被改动过就要重新生成并同步这里");
        let t = sheet(&b, 200, 20).unwrap();
        assert!(t.body.trim().is_empty(), "空表不该产正文：{:?}", t.body);
        assert_eq!((t.total, t.rows, t.cols), (0, 0, 0));
    }
}
