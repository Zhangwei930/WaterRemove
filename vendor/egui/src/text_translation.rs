//! mImageViewer local patch: optional translation hook for UI text.
//!
//! The application registers a translator with [`set_text_translator`]. egui then
//! passes widget text (labels, buttons, window titles, tooltips, menus, …) and text
//! laid out through [`crate::Painter`] through it right before layout, so the app can
//! swap its hard-coded source strings for another language without touching every
//! call site.
//!
//! Text typed into a [`crate::TextEdit`] is never translated: `TextEdit` lays out its
//! buffer with its own layouter, which does not go through this hook.
//!
//! Widget ids are derived from the source text, not from the translated text, so
//! switching languages does not change any widget id.
//!
//! Code that paints user content (file names, tags, …) can hold a
//! [`NoTranslationGuard`] so that such text is never mistaken for a UI string. Explicit
//! calls to [`translate_text`] still translate while a guard is held.

use std::cell::Cell;
use std::sync::{Arc, RwLock};

use epaint::text::LayoutJob;

/// Returns `Some(translated)` when the given source text has a translation.
pub type TextTranslator = dyn Fn(&str) -> Option<String> + Send + Sync;

static TRANSLATOR: RwLock<Option<Arc<TextTranslator>>> = RwLock::new(None);

thread_local! {
    static NO_TRANSLATION_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// While alive, egui does not translate text on this thread on its own
/// (widget text and [`crate::Painter`] layout). Use it around code that paints user
/// content such as file names.
#[must_use = "translation is suppressed only while the guard is alive"]
pub struct NoTranslationGuard {
    // Tied to the thread whose counter it incremented.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl NoTranslationGuard {
    #[expect(clippy::new_without_default)]
    pub fn new() -> Self {
        NO_TRANSLATION_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for NoTranslationGuard {
    fn drop(&mut self) {
        NO_TRANSLATION_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn automatic_translation_suppressed() -> bool {
    NO_TRANSLATION_DEPTH.with(|depth| depth.get() > 0)
}

/// Install (`Some`) or remove (`None`) the process-wide text translator.
///
/// Galleys are cached by their final text, so the new language takes effect on the
/// next frame without clearing any cache. Call [`crate::Context::request_repaint`]
/// after switching so that the next frame happens promptly.
pub fn set_text_translator(translator: Option<Arc<TextTranslator>>) {
    let mut slot = TRANSLATOR
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = translator;
}

/// Is a translator currently installed?
pub fn has_text_translator() -> bool {
    current_translator().is_some()
}

fn current_translator() -> Option<Arc<TextTranslator>> {
    TRANSLATOR
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Translate one piece of text with the installed translator.
///
/// Pure ASCII text is returned untranslated without consulting the translator.
pub fn translate_text(text: &str) -> Option<String> {
    if text.is_ascii() {
        return None;
    }
    let translator = current_translator()?;
    translator(text).filter(|translated| translated != text)
}

/// Translate `text` in place, as egui does for widget text. Returns `true` if it
/// changed. Does nothing while a [`NoTranslationGuard`] is held.
pub fn translate_string(text: &mut String) -> bool {
    if automatic_translation_suppressed() {
        return false;
    }
    match translate_text(text) {
        Some(translated) => {
            *text = translated;
            true
        }
        None => false,
    }
}

/// Translate every section of a [`LayoutJob`] in place, keeping each section's format.
///
/// Each section is translated on its own, so a job made of differently colored parts
/// keeps its colors. Returns `true` if any section changed. Does nothing while a
/// [`NoTranslationGuard`] is held.
pub fn translate_layout_job(job: &mut LayoutJob) -> bool {
    if job.text.is_ascii() || automatic_translation_suppressed() {
        return false;
    }
    let Some(translator) = current_translator() else {
        return false;
    };
    let translate = |text: &str| -> Option<String> {
        if text.is_ascii() {
            return None;
        }
        translator(text).filter(|translated| translated != text)
    };

    if job.sections.len() <= 1 {
        let covers_all = job
            .sections
            .first()
            .is_none_or(|section| section.byte_range == (0..job.text.len()));
        if covers_all {
            let Some(translated) = translate(&job.text) else {
                return false;
            };
            if let Some(section) = job.sections.first_mut() {
                section.byte_range = 0..translated.len();
            }
            job.text = translated;
            return true;
        }
    }

    // Sections must be in order and cover disjoint ranges to be rebuilt safely.
    let mut previous_end = 0;
    for section in &job.sections {
        if section.byte_range.start < previous_end
            || section.byte_range.end < section.byte_range.start
            || section.byte_range.end > job.text.len()
            || !job.text.is_char_boundary(section.byte_range.start)
            || !job.text.is_char_boundary(section.byte_range.end)
        {
            return false;
        }
        previous_end = section.byte_range.end;
    }

    let mut changed = false;
    let mut new_text = String::with_capacity(job.text.len());
    let mut copied_until = 0;
    for section in &mut job.sections {
        let range = section.byte_range.clone();
        // Text between sections is not rendered, but keep it so nothing shifts.
        new_text.push_str(&job.text[copied_until..range.start]);
        let source = &job.text[range.clone()];
        let start = new_text.len();
        match translate(source) {
            Some(translated) => {
                new_text.push_str(&translated);
                changed = true;
            }
            None => new_text.push_str(source),
        }
        section.byte_range = start..new_text.len();
        copied_until = range.end;
    }
    new_text.push_str(&job.text[copied_until..]);
    if changed {
        job.text = new_text;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use epaint::text::TextFormat;
    use std::sync::Mutex;

    // The translator is process-wide; keep these tests from racing each other.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn install_test_translator() {
        set_text_translator(Some(Arc::new(|text: &str| match text {
            "設定" => Some("设置".to_owned()),
            "閉じる" => Some("关闭".to_owned()),
            _ => None,
        })));
    }

    #[test]
    fn translates_single_section_job_and_keeps_range_consistent() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install_test_translator();
        let mut job =
            LayoutJob::simple_singleline("設定".to_owned(), Default::default(), Default::default());
        assert!(translate_layout_job(&mut job));
        assert_eq!(job.text, "设置");
        assert_eq!(job.sections[0].byte_range, 0..job.text.len());
        set_text_translator(None);
    }

    #[test]
    fn translates_each_section_separately() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install_test_translator();
        let mut job = LayoutJob::default();
        job.append("設定", 0.0, TextFormat::default());
        job.append(" / ", 0.0, TextFormat::default());
        job.append("閉じる", 0.0, TextFormat::default());
        assert!(translate_layout_job(&mut job));
        assert_eq!(job.text, "设置 / 关闭");
        let parts: Vec<&str> = job
            .sections
            .iter()
            .map(|section| &job.text[section.byte_range.clone()])
            .collect();
        assert_eq!(parts, ["设置", " / ", "关闭"]);
        set_text_translator(None);
    }

    #[test]
    fn guard_suppresses_automatic_translation_only() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install_test_translator();
        {
            let _no_translation = NoTranslationGuard::new();
            let mut text = "設定".to_owned();
            assert!(!translate_string(&mut text));
            assert_eq!(text, "設定");
            // Explicit lookups still work.
            assert_eq!(translate_text("設定").as_deref(), Some("设置"));
        }
        let mut text = "設定".to_owned();
        assert!(translate_string(&mut text));
        assert_eq!(text, "设置");
        set_text_translator(None);
    }

    #[test]
    fn untranslated_and_ascii_text_is_left_alone() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install_test_translator();
        assert_eq!(translate_text("Settings"), None);
        assert_eq!(translate_text("未登録"), None);
        set_text_translator(None);
        assert_eq!(translate_text("設定"), None);
    }
}
