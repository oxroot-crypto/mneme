use std::sync::Arc;

thread_local! {
    /// 最近一次 Glob 模式的字符表:同一过滤表达式逐行求值时模式不变,
    /// 缓存后每行只收集待匹配文本,免重复解析模式串。
    static GLOB_PATTERN: std::cell::RefCell<Option<(Arc<str>, Vec<char>)>> =
        const { std::cell::RefCell::new(None) };
}

/// 通配符匹配:`*` 匹配任意串,`?` 匹配单个字符。
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let txt: Vec<char> = text.chars().collect();
    GLOB_PATTERN.with(|cell| match cell.try_borrow_mut() {
        Ok(mut slot) => {
            let hit = slot
                .as_ref()
                .is_some_and(|(cached, _)| cached.as_ref() == pattern);
            if !hit {
                *slot = Some((Arc::from(pattern), pattern.chars().collect()));
            }
            let pat: &[char] = match slot.as_ref() {
                Some((_, chars)) => chars.as_slice(),
                None => &[],
            };
            glob_match_chars(pat, &txt)
        }
        // reason: 重入借用冲突时一次性解析模式串,语义不变(仅多一次分配)。
        Err(_) => glob_match_chars(&pattern.chars().collect::<Vec<_>>(), &txt),
    })
}

/// 双指针回溯匹配主体(经典 $O(n\cdot m)$ 最坏;模式串短时开销可忽略)。
fn glob_match_chars(pat: &[char], txt: &[char]) -> bool {
    let (mut p, mut t) = (0_usize, 0_usize);
    let mut star: Option<usize> = None;
    let mut star_match = 0_usize;
    while t < txt.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == txt[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            star_match = t;
            p += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_match += 1;
            t = star_match;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}
