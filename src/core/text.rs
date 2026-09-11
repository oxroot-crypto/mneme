//! 文本分词(自研,零依赖;设计 06 §3.5)。
//!
//! 规则:按 Unicode 空白切词 → 去首尾标点 → 小写化;连续 CJK 段做 bigram
//! ("记忆库" → "记忆"、"忆库"),拉丁/数字词保持整词。停用词表为内置常量,
//! 经 [`tokenize`] 的开关控制(对应 `Tuning::stopwords`)。
//!
//! 未来替换 jieba 级分词器只动本模块,BM25 与倒排构建均经 [`tokenize`] 调用。

/// 内置停用词(小写;拉丁虚词 + 常见 CJK 虚字)。
///
/// 词表保持短小:停用词只影响关键词通道的区分度,不影响召回正确性。
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "he", "in", "is", "it",
    "its", "of", "on", "or", "that", "the", "to", "was", "were", "will", "with", "的", "了", "是",
    "在", "和", "与", "或",
];

/// 判断字符是否属于 CJK 统一表意文字(基本区、扩展 A、兼容区与扩展 B 起)。
///
/// # Arguments
/// * `c` - 待判断字符。
///
/// # Returns
/// 属于 CJK 表意文字时返回 `true`;日文假名与韩文不在内(按整词处理)。
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x2_0000..=0x2_FA1F
    )
}

/// 判断 token 是否命中内置停用词表。
fn is_stopword(token: &str) -> bool {
    STOPWORDS.contains(&token)
}

/// 把一个连续 CJK 段拆为 bigram(长度为 1 时保留单字)。
fn push_cjk_run(run: &str, stopwords_enabled: bool, out: &mut Vec<String>) {
    let chars: Vec<char> = run.chars().collect();
    if chars.len() == 1 {
        let token = chars[0].to_string();
        if !(stopwords_enabled && is_stopword(&token)) {
            out.push(token);
        }
        return;
    }
    for pair in chars.windows(2) {
        let token: String = pair.iter().collect();
        if !(stopwords_enabled && is_stopword(&token)) {
            out.push(token);
        }
    }
}

/// 切分一个已去首尾标点的空白段:CJK 连续段做 bigram,其余整词小写化。
fn tokenize_segment(segment: &str, stopwords_enabled: bool, out: &mut Vec<String>) {
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        if is_cjk(c) {
            let mut run = String::new();
            run.push(c);
            while let Some(&next) = chars.peek() {
                if is_cjk(next) {
                    run.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            push_cjk_run(&run, stopwords_enabled, out);
        } else {
            let mut run = String::new();
            run.push(c);
            while let Some(&next) = chars.peek() {
                if is_cjk(next) {
                    break;
                }
                run.push(next);
                chars.next();
            }
            let token = run.to_lowercase();
            if !(stopwords_enabled && is_stopword(&token)) {
                out.push(token);
            }
        }
    }
}

/// 把文本切分为检索词条序列。
///
/// # Arguments
/// * `text` - 原始文本。
/// * `stopwords_enabled` - 是否启用内置停用词过滤(对应 `Tuning::stopwords`)。
///
/// # Returns
/// 按出现顺序排列的词条;空白输入返回空 `Vec`。
///
/// # Examples
/// ```
/// use mneme::tokenize;
///
/// let tokens = tokenize("Memory 记忆库", true);
/// assert!(tokens.contains(&"memory".to_string()));
/// assert!(tokens.contains(&"记忆".to_string()));
/// assert!(tokens.contains(&"忆库".to_string()));
/// ```
pub fn tokenize(text: &str, stopwords_enabled: bool) -> Vec<String> {
    let mut tokens = Vec::new();
    for segment in text.split_whitespace() {
        let trimmed = segment.trim_matches(|c: char| !c.is_alphanumeric());
        if trimmed.is_empty() {
            continue;
        }
        tokenize_segment(trimmed, stopwords_enabled, &mut tokens);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_words_are_lowercased_and_punctuation_trimmed() {
        assert_eq!(tokenize("Hello, World!", true), vec!["hello", "world"]);
    }

    #[test]
    fn cjk_runs_become_bigrams() {
        assert_eq!(
            tokenize("记忆库", false),
            vec!["记忆".to_string(), "忆库".to_string()]
        );
    }

    #[test]
    fn single_cjk_char_is_kept() {
        assert_eq!(tokenize("记", false), vec!["记"]);
    }

    #[test]
    fn mixed_script_splits_at_script_boundary() {
        assert_eq!(
            tokenize("abc记忆x", false),
            vec!["abc".to_string(), "记忆".to_string(), "x".to_string()]
        );
    }

    #[test]
    fn stopwords_are_filtered_only_when_enabled() {
        assert_eq!(tokenize("the memory", true), vec!["memory"]);
        assert_eq!(tokenize("the memory", false), vec!["the", "memory"]);
    }

    #[test]
    fn whitespace_only_text_yields_no_tokens() {
        assert!(tokenize("   ", true).is_empty());
    }

    /// FC-CORE-POST-008(纯标点输入不产生词条)
    #[test]
    fn punctuation_only_text_yields_no_tokens() {
        assert!(tokenize("... ,.;!?", true).is_empty());
        assert!(tokenize("... ,.;!?", false).is_empty());
    }
}
