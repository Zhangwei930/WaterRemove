//! UI 表示言語の切替。
//!
//! ソースの UI 文字列は日本語のまま持ち、翻訳は egui の描画直前に行う
//! (`vendor/egui/src/text_translation.rs` のフック)。ここではその翻訳関数を
//! 言語ごとの翻訳表から作って登録する。翻訳表は `assets/i18n/<言語>.json`。
//!
//! 翻訳表の形式:
//!
//! ```json
//! {
//!   "strings":  { "設定": "设置" },
//!   "patterns": { "{0} 件を削除しますか？": "要删除 {0} 项吗？" }
//! }
//! ```
//!
//! - `strings` は描画される文字列全体との完全一致。
//! - `patterns` は `format!` で組み立てる文字列用。`{0}` `{1}` … が可変部分で、
//!   訳文では順番を入れ替えてよい。可変部分そのものは、`strings` に完全一致する
//!   ときだけ訳す (ファイル名などの利用者の文字列を書き換えないため)。
//! - 固定部分が記号だけのパターン (`"{0} - {1}"` など) は、可変部分のどれかが
//!   `strings` で訳せたときだけ使う。ラベルをつなぐだけの `format!` 用。
//! - 固定部分に仮名が無いパターン (`"{0}件"` `"{0}以上"` など) は、漢字が中国語と
//!   共通なので中国語の文字列にも一致してしまう。可変部分が数字などの漢字・仮名を
//!   含まない値か、`strings` で訳せる値のときだけ使う。
//! - 訳した結果はメモに「訳さない」として記録し、同じ文字列が再び渡されても
//!   二重に訳さない。
//! - どちらにも無い文字列は、前後の空白を除いた本体、または行ごとに引き直す。
//!   それでも見つからなければ日本語のまま表示する。
//!
//! 翻訳表の抽出・検査は `scripts/i18n_tool.py` を使う ([docs/i18n.md](../docs/i18n.md))。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::settings::UiLanguage;

const ZH_HANS_CATALOG_JSON: &str = include_str!("../assets/i18n/zh-Hans.json");

/// 可変文字列 (進捗の数字など) が毎フレーム変わっても無制限に増えないよう、
/// 引き直し結果のメモはこの件数を超えたら捨てる。
const MEMO_LIMIT: usize = 20_000;

const LANGUAGE_UNSET: u8 = u8::MAX;
static APPLIED_LANGUAGE: AtomicU8 = AtomicU8::new(LANGUAGE_UNSET);

fn language_code(language: UiLanguage) -> u8 {
    match language.normalized() {
        UiLanguage::Japanese => 0,
        UiLanguage::SimplifiedChinese => 1,
        UiLanguage::Unknown => unreachable!("normalized ui language"),
    }
}

/// 表示言語を切り替える。前回と同じ言語なら何もしない (毎フレーム呼んでよい)。
///
/// 切り替えたときは `true` を返す。呼び出し側は再描画を要求すること。
pub fn apply_language(language: UiLanguage) -> bool {
    let code = language_code(language);
    if APPLIED_LANGUAGE.swap(code, Ordering::AcqRel) == code {
        return false;
    }
    let translator: Option<Arc<egui::text_translation::TextTranslator>> =
        match language.normalized() {
            UiLanguage::Japanese | UiLanguage::Unknown => None,
            UiLanguage::SimplifiedChinese => {
                let catalog = zh_hans_catalog();
                Some(Arc::new(move |text: &str| catalog.translate(text)))
            }
        };
    egui::text_translation::set_text_translator(translator);
    true
}

/// egui を通らない場所 (検索の照合など) 用に、現在の言語で文字列を訳す。
pub fn tr(text: &str) -> std::borrow::Cow<'_, str> {
    match egui::text_translation::translate_text(text) {
        Some(translated) => std::borrow::Cow::Owned(translated),
        None => std::borrow::Cow::Borrowed(text),
    }
}

fn zh_hans_catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| match Catalog::parse(ZH_HANS_CATALOG_JSON) {
        Ok(catalog) => catalog,
        Err(error) => {
            crate::logger::log(format!("i18n: zh-Hans catalog is invalid: {error}"));
            Catalog::default()
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Hole(usize),
}

#[derive(Debug)]
struct Pattern {
    source: Vec<Segment>,
    target: Vec<Segment>,
    hole_count: usize,
    /// 固定部分の総バイト数。長い (= 具体的な) パターンを先に試す。
    literal_len: usize,
    /// 固定部分が記号だけ。可変部分のどれかが訳せたときだけ使う。
    generic: bool,
    /// 固定部分に仮名が無い。可変部分は漢字・仮名を含まない値か訳せる値に限る。
    kanji_only: bool,
}

#[derive(Default)]
struct Catalog {
    strings: HashMap<String, String>,
    /// `strings` の訳文。描画経路で訳文が再び渡されても訳し直さない。
    outputs: HashSet<String>,
    patterns: Vec<Pattern>,
    memo: Mutex<HashMap<String, Option<String>>>,
}

#[derive(serde::Deserialize)]
struct CatalogFile {
    #[serde(default)]
    strings: HashMap<String, String>,
    #[serde(default)]
    patterns: HashMap<String, String>,
}

impl Catalog {
    fn parse(json: &str) -> Result<Self, String> {
        let file: CatalogFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut patterns = Vec::with_capacity(file.patterns.len());
        for (source, target) in &file.patterns {
            patterns.push(
                Pattern::parse(source, target).map_err(|e| format!("pattern {source:?}: {e}"))?,
            );
        }
        patterns.sort_by(|a, b| {
            b.literal_len
                .cmp(&a.literal_len)
                .then_with(|| a.hole_count.cmp(&b.hole_count))
        });
        let strings: HashMap<String, String> = file
            .strings
            .into_iter()
            .filter(|(source, target)| !source.is_empty() && !target.is_empty())
            .collect();
        let outputs = strings
            .values()
            .filter(|target| !strings.contains_key(*target))
            .cloned()
            .collect();
        Ok(Self {
            strings,
            outputs,
            patterns,
            memo: Mutex::new(HashMap::new()),
        })
    }

    fn translate(&self, text: &str) -> Option<String> {
        if let Some(translated) = self.strings.get(text) {
            return Some(translated.clone());
        }
        if self.outputs.contains(text)
            || (self.patterns.is_empty() && !text.contains('\n') && text.trim() == text)
        {
            return None;
        }
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = memo.get(text) {
            return cached.clone();
        }
        let result = self.translate_uncached(text);
        if memo.len() >= MEMO_LIMIT {
            memo.clear();
        }
        memo.insert(text.to_owned(), result.clone());
        if let Some(translated) = &result {
            // 訳文がもう一度描画経路を通っても訳し直さない。
            memo.insert(translated.clone(), None);
        }
        result
    }

    fn translate_uncached(&self, text: &str) -> Option<String> {
        if let Some(translated) = self.translate_line(text) {
            return Some(translated);
        }
        if !text.contains('\n') {
            return None;
        }
        // 複数の文言を改行でつないだ説明文は、行ごとに引く。
        let mut changed = false;
        let lines: Vec<String> = text
            .split('\n')
            .map(|line| match self.translate_line(line) {
                Some(translated) => {
                    changed = true;
                    translated
                }
                None => line.to_owned(),
            })
            .collect();
        changed.then(|| lines.join("\n"))
    }

    fn translate_line(&self, text: &str) -> Option<String> {
        if let Some(translated) = self.translate_core(text) {
            return Some(translated);
        }
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.len() == text.len() {
            return None;
        }
        let start = text.len() - text.trim_start().len();
        let end = start + trimmed.len();
        let translated = self.translate_core(trimmed)?;
        Some(format!("{}{translated}{}", &text[..start], &text[end..]))
    }

    fn translate_core(&self, text: &str) -> Option<String> {
        if text.is_ascii() {
            return None;
        }
        if let Some(translated) = self.strings.get(text) {
            return Some(translated.clone());
        }
        self.patterns.iter().find_map(|pattern| {
            let captures = pattern.match_text(text)?;
            let mut any_translated = false;
            let mut untranslated_cjk = false;
            let captures: Vec<String> = captures
                .into_iter()
                .map(|capture| match self.strings.get(capture) {
                    Some(translated) if translated != capture => {
                        any_translated = true;
                        translated.clone()
                    }
                    _ => {
                        untranslated_cjk |= capture.chars().any(is_cjk_or_kana);
                        capture.to_owned()
                    }
                })
                .collect();
            if pattern.generic && !any_translated {
                return None;
            }
            if pattern.kanji_only && !pattern.generic && untranslated_cjk {
                return None;
            }
            Some(pattern.render(&captures))
        })
    }
}

impl Pattern {
    fn parse(source: &str, target: &str) -> Result<Self, String> {
        let source_segments = parse_segments(source)?;
        let target_segments = parse_segments(target)?;
        let mut holes: Vec<usize> = source_segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Hole(index) => Some(*index),
                Segment::Literal(_) => None,
            })
            .collect();
        let hole_count = holes.len();
        holes.sort_unstable();
        if holes.iter().copied().ne(0..hole_count) {
            return Err("holes must be {0}, {1}, … each used once".to_owned());
        }
        for segment in &target_segments {
            if let Segment::Hole(index) = segment
                && *index >= hole_count
            {
                return Err(format!("translation uses unknown hole {{{index}}}"));
            }
        }
        if source_segments
            .windows(2)
            .any(|pair| matches!(pair, [Segment::Hole(_), Segment::Hole(_)]))
        {
            return Err("adjacent holes cannot be matched unambiguously".to_owned());
        }
        let literal_len: usize = source_segments
            .iter()
            .map(|segment| match segment {
                Segment::Literal(text) => text.len(),
                Segment::Hole(_) => 0,
            })
            .sum();
        let literal_chars = || {
            source_segments.iter().flat_map(|segment| match segment {
                Segment::Literal(text) => text.chars().collect::<Vec<_>>(),
                Segment::Hole(_) => Vec::new(),
            })
        };
        if hole_count == 0 || literal_chars().all(char::is_whitespace) {
            return Err("pattern needs holes and a visible fixed part".to_owned());
        }
        let generic = literal_chars().all(|ch| !ch.is_alphanumeric());
        let kanji_only = !literal_chars().any(is_kana);
        Ok(Self {
            source: source_segments,
            target: target_segments,
            hole_count,
            literal_len,
            generic,
            kanji_only,
        })
    }

    /// 一致すれば可変部分を `{0}` `{1}` … の順で返す。可変部分は空文字列を許さない。
    fn match_text<'a>(&self, text: &'a str) -> Option<Vec<&'a str>> {
        let mut captures: Vec<Option<&'a str>> = vec![None; self.hole_count];
        let mut cursor = 0;
        let mut pending_hole: Option<usize> = None;
        let last = self.source.len().saturating_sub(1);
        for (position, segment) in self.source.iter().enumerate() {
            match segment {
                Segment::Hole(index) => pending_hole = Some(*index),
                Segment::Literal(literal) => {
                    let found = match pending_hole {
                        None => {
                            if !text[cursor..].starts_with(literal.as_str()) {
                                return None;
                            }
                            cursor
                        }
                        Some(hole) => {
                            let search_from = next_char_boundary(text, cursor)?;
                            let found = if position == last {
                                // 末尾の固定部分は必ず文字列の末尾に一致させる。
                                let start = text.len().checked_sub(literal.len())?;
                                (start >= search_from
                                    && text.is_char_boundary(start)
                                    && text[start..] == **literal)
                                    .then_some(start)?
                            } else {
                                search_from + text[search_from..].find(literal.as_str())?
                            };
                            captures[hole] = Some(&text[cursor..found]);
                            pending_hole = None;
                            found
                        }
                    };
                    cursor = found + literal.len();
                }
            }
        }
        match pending_hole {
            Some(hole) => {
                if cursor >= text.len() {
                    return None;
                }
                captures[hole] = Some(&text[cursor..]);
            }
            None => {
                if cursor != text.len() {
                    return None;
                }
            }
        }
        captures.into_iter().collect()
    }

    fn render(&self, captures: &[String]) -> String {
        let mut out = String::new();
        for segment in &self.target {
            match segment {
                Segment::Literal(text) => out.push_str(text),
                Segment::Hole(index) => out.push_str(&captures[*index]),
            }
        }
        out
    }
}

fn is_kana(ch: char) -> bool {
    matches!(ch, '\u{3041}'..='\u{30ff}' | '\u{ff66}'..='\u{ff9f}') && ch != '・' && ch != 'ー'
}

fn is_cjk_or_kana(ch: char) -> bool {
    is_kana(ch)
        || matches!(ch, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

/// 可変部分の最初の 1 文字を必ず含めるため、次の文字境界を返す。
fn next_char_boundary(text: &str, from: usize) -> Option<usize> {
    let ch = text[from..].chars().next()?;
    Some(from + ch.len_utf8())
}

/// `{0}` を可変部分として、`{{` `}}` を波括弧そのものとして読む。
fn parse_segments(text: &str) -> Result<Vec<Segment>, String> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                let mut digits = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(d) if d.is_ascii_digit() => digits.push(d),
                        _ => return Err(format!("bad placeholder in {text:?}")),
                    }
                }
                let index = digits
                    .parse()
                    .map_err(|_| format!("bad placeholder in {text:?}"))?;
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(Segment::Hole(index));
            }
            '}' => return Err(format!("unmatched '}}' in {text:?}")),
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(json: &str) -> Catalog {
        Catalog::parse(json).expect("valid catalog")
    }

    #[test]
    fn bundled_zh_hans_catalog_is_valid() {
        let catalog = Catalog::parse(ZH_HANS_CATALOG_JSON).expect("zh-Hans.json");
        assert!(!catalog.strings.is_empty());
    }

    #[test]
    fn exact_trimmed_and_multiline_lookup() {
        let catalog =
            catalog(r#"{"strings": {"設定": "设置", "閉じる": "关闭", "テーマ": "主题"}}"#);
        assert_eq!(catalog.translate("設定").as_deref(), Some("设置"));
        assert_eq!(catalog.translate("  設定 ").as_deref(), Some("  设置 "));
        assert_eq!(
            catalog.translate("テーマ\n未訳の行\n閉じる").as_deref(),
            Some("主题\n未訳の行\n关闭")
        );
        assert_eq!(catalog.translate("未訳"), None);
    }

    #[test]
    fn patterns_capture_and_reorder_holes() {
        let catalog = catalog(
            r#"{
                "strings": {"サムネイル": "缩略图"},
                "patterns": {
                    "{0} 件を削除しますか？": "要删除 {0} 项吗？",
                    "{0} を {1} に移動": "将 {0} 移到 {1}",
                    "{0} を表示": "显示{0}"
                }
            }"#,
        );
        assert_eq!(
            catalog.translate("12 件を削除しますか？").as_deref(),
            Some("要删除 12 项吗？")
        );
        assert_eq!(
            catalog.translate("a.jpg を B に移動").as_deref(),
            Some("将 a.jpg 移到 B")
        );
        // 可変部分は完全一致する訳があるときだけ訳す。
        assert_eq!(
            catalog.translate("サムネイル を表示").as_deref(),
            Some("显示缩略图")
        );
        assert_eq!(
            catalog.translate("写真 を表示").as_deref(),
            Some("显示写真")
        );
        // 可変部分は空にならない。
        assert_eq!(catalog.translate(" を表示"), None);
        // 末尾の固定部分より短い位置で文字の途中を切らない。
        assert_eq!(catalog.translate("キーを表示"), None);
    }

    #[test]
    fn kanji_only_patterns_do_not_rewrite_chinese_text() {
        let catalog = catalog(
            r#"{
                "strings": {"画像": "图片"},
                "patterns": {"{0}件": "{0} 项", "{0}以上": "{0} 以上"}
            }"#,
        );
        assert_eq!(catalog.translate("12件").as_deref(), Some("12 项"));
        assert_eq!(catalog.translate("画像件").as_deref(), Some("图片 项"));
        // 中国語の文字列 (利用者のファイル名など) は書き換えない。
        assert_eq!(catalog.translate("打开文件"), None);
        assert_eq!(catalog.translate("三年以上"), None);
    }

    #[test]
    fn translated_output_is_not_translated_again() {
        let catalog = catalog(
            r#"{"strings": {"以上": "以上", "件": "项", "ファイル": "文件"},
                "patterns": {"{0}以上": "{0} 以上", "{0}件": "{0} 项"}}"#,
        );
        let once = catalog.translate("100MB以上").unwrap();
        assert_eq!(once, "100MB 以上");
        assert_eq!(catalog.translate(&once), None);
        // 完全一致の訳文も訳し直さない。
        assert_eq!(catalog.translate("文件"), None);
    }

    #[test]
    fn longer_patterns_win() {
        let catalog =
            catalog(r#"{"patterns": {"{0}件": "{0} 项", "{0}件のファイル": "{0} 个文件"}}"#);
        assert_eq!(
            catalog.translate("3件のファイル").as_deref(),
            Some("3 个文件")
        );
        assert_eq!(catalog.translate("3件").as_deref(), Some("3 项"));
    }

    #[test]
    fn symbol_only_patterns_need_a_translated_hole() {
        let catalog = catalog(
            r#"{
                "strings": {"軽量": "轻量", "高速です": "很快"},
                "patterns": {"{0} - {1}": "{0} - {1}", "{0}（{1}）": "{0}（{1}）"}
            }"#,
        );
        assert_eq!(
            catalog.translate("軽量 - 高速です").as_deref(),
            Some("轻量 - 很快")
        );
        assert_eq!(
            catalog.translate("軽量（未訳）").as_deref(),
            Some("轻量（未訳）")
        );
        assert_eq!(catalog.translate("未訳 - 未訳"), None);
    }

    #[test]
    fn invalid_patterns_are_rejected() {
        assert!(Pattern::parse("{0}{1}件", "{0}{1}").is_err());
        assert!(Pattern::parse("{0} {1}", "{0} {1}").is_err());
        assert!(Pattern::parse("{0}件", "{1}").is_err());
        assert!(Pattern::parse("{1}件", "{1}").is_err());
        assert!(Pattern::parse("{{0}}件", "{{0}}").is_err());
        assert!(Pattern::parse("{{{0}}}件", "「{0}」").is_ok());
    }
}
