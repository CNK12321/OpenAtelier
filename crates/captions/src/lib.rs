//! Captions from speech: transcribing with faster-whisper, then grouping the words into
//! captions.
//!
//! Transcription isn't built into the editor — it would add a Python runtime and a
//! model of hundreds of megabytes to every install. The **caption engine** ([`engine`])
//! is downloaded on request into its own folder (a private Python, faster-whisper and
//! the chosen model), used from there, and removable in one go.
//!
//! Everything else is plain Rust: the engine's output ([`Event`]) becomes timed
//! [`Word`]s, and [`group`] turns them into captions by the user's rules — how many
//! words, how long, where silences and punctuation break them.

pub mod engine;

use serde::Deserialize;

/// A Whisper model the engine can download, with roughly how much it takes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub name: &'static str,
    pub size_mb: u32,
    pub about: &'static str,
}

/// The models offered, smallest first.
pub const MODELS: [ModelInfo; 7] = [
    ModelInfo { name: "tiny", size_mb: 75, about: "Fastest, least accurate" },
    ModelInfo { name: "base", size_mb: 145, about: "Fast, rough" },
    ModelInfo { name: "small", size_mb: 485, about: "A good balance (recommended)" },
    ModelInfo { name: "medium", size_mb: 1530, about: "More accurate, slower" },
    ModelInfo { name: "turbo", size_mb: 1620, about: "Near large-v3 accuracy, much faster" },
    ModelInfo { name: "large-v3", size_mb: 3100, about: "Most accurate, slowest" },
    ModelInfo { name: "small.en", size_mb: 485, about: "English only, a little better at it" },
];

pub fn model(name: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.name == name)
}

/// One transcribed word, in seconds from the start of the audio.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    #[serde(rename = "word")]
    pub text: String,
    /// The model's confidence, 0–1.
    #[serde(default, rename = "p")]
    pub probability: f64,
}

/// One line of the engine's output.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    /// The language it heard (or was told) and the audio's length.
    Info { language: String, duration: f64 },
    /// A stretch of speech with its words (they arrive as the model gets through them).
    Segment { start: f64, end: f64, text: String, words: Vec<Word> },
    Done { path: String },
    Error { message: String },
}

impl Event {
    /// Parses a line of engine output; anything that isn't an event (a library's log
    /// line) is `None`.
    pub fn parse(line: &str) -> Option<Event> {
        let line = line.trim();
        if !line.starts_with('{') {
            return None;
        }
        serde_json::from_str(line).ok()
    }
}

/// How words become captions.
#[derive(Clone, Debug, PartialEq, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct Grouping {
    pub mode: Mode,
    /// At most this many words on screen at once.
    pub max_words: usize,
    /// No caption lasts longer than this (seconds).
    pub max_seconds: f64,
    /// A pause at least this long (seconds) ends a caption. Shorter gaps between captions
    /// are closed, so the text doesn't flicker off between them.
    pub silence: f64,
    /// Captions stay up at least this long (seconds), if the next one leaves room.
    pub min_seconds: f64,
    pub uppercase: bool,
    /// Drop commas and periods (question and exclamation marks stay).
    pub strip_punctuation: bool,
}

impl Default for Grouping {
    fn default() -> Self {
        Grouping { mode: Mode::Phrases, max_words: 6, max_seconds: 3.0, silence: 0.6, min_seconds: 0.4, uppercase: false, strip_punctuation: false }
    }
}

/// Where captions may break, besides the limits.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Fill each caption up to the limits.
    Words,
    /// Also break after commas and sentence ends.
    #[default]
    Phrases,
    /// Also break after sentence ends only.
    Sentences,
    /// One word at a time (the punchy short-form style).
    Single,
}

/// One caption: its time on the audio's clock, its text, and the words it came from
/// (indexes into the word list, so colors set on words survive regrouping).
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: std::ops::Range<usize>,
}

fn ends_phrase(word: &str) -> bool {
    word.trim_end().ends_with([',', ';', ':', '.', '?', '!', '…'])
}

fn ends_sentence(word: &str) -> bool {
    word.trim_end().ends_with(['.', '?', '!', '…'])
}

/// Hand-made changes to the grouping, by word index, so they survive changing the rules:
/// a caption always starts at a word in `breaks` (split there), and never at a word in
/// `joins` (joined to the one before, whatever the limits say).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manual {
    pub breaks: std::collections::BTreeSet<usize>,
    pub joins: std::collections::BTreeSet<usize>,
}

impl Manual {
    /// Split so a caption starts at word `at`.
    pub fn split(&mut self, at: usize) {
        self.joins.remove(&at);
        self.breaks.insert(at);
    }

    /// Join the caption starting at word `at` onto the one before it.
    pub fn join(&mut self, at: usize) {
        self.breaks.remove(&at);
        self.joins.insert(at);
    }
}

/// Groups `words` (in time order) into captions by `g`.
pub fn group(words: &[Word], g: &Grouping) -> Vec<Caption> {
    group_with(words, g, &Manual::default())
}

/// [`group`], with hand-made splits and joins.
pub fn group_with(words: &[Word], g: &Grouping, manual: &Manual) -> Vec<Caption> {
    let max_words = if g.mode == Mode::Single { 1 } else { g.max_words.max(1) };
    let max_seconds = if g.max_seconds > 0.0 { g.max_seconds } else { f64::INFINITY };
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    let mut current: Option<std::ops::Range<usize>> = None;
    for (i, w) in words.iter().enumerate() {
        if w.text.trim().is_empty() {
            continue;
        }
        if let Some(span) = &current {
            let first = &words[span.start];
            let prev = &words[span.end - 1];
            let count = words[span.clone()].iter().filter(|w| !w.text.trim().is_empty()).count();
            let punctuated = match g.mode {
                Mode::Phrases => ends_phrase(&prev.text),
                Mode::Sentences => ends_sentence(&prev.text),
                _ => false,
            };
            let too_long = w.end - first.start > max_seconds;
            let pause = w.start - prev.end >= g.silence && g.silence > 0.0;
            let by_rules = count >= max_words || punctuated || too_long || pause;
            if manual.breaks.contains(&i) || (by_rules && !manual.joins.contains(&i)) {
                spans.extend(current.take());
            }
        }
        current = Some(match current.take() {
            Some(span) => span.start..i + 1,
            None => i..i + 1,
        });
    }
    spans.extend(current);

    let mut out: Vec<Caption> = spans
        .into_iter()
        .map(|span| {
            let text = words[span.clone()].iter().map(|w| w.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
            let text = if g.strip_punctuation { text.replace([',', '.', ';', ':'], "") } else { text };
            let text = if g.uppercase { text.to_uppercase() } else { text };
            Caption { start: words[span.start].start, end: words[span.end - 1].end, text, words: span }
        })
        .collect();

    // Timing: short gaps close (no flicker between captions), and each stays up for
    // `min_seconds` where the next one leaves room.
    for i in 0..out.len() {
        let next = out.get(i + 1).map(|n| n.start);
        let c = &mut out[i];
        if let Some(next) = next
            && next - c.end < g.silence
        {
            c.end = next;
        }
        let want = c.start + g.min_seconds;
        if c.end < want {
            c.end = next.map_or(want, |n| want.min(n));
        }
        // Words can overlap a little in Whisper's timing: never into the next caption.
        if let Some(next) = next.filter(|n| *n > c.start) {
            c.end = c.end.min(next);
        }
        if c.end <= c.start {
            c.end = c.start + 0.05;
        }
    }
    out
}

/// The words of a whole transcript, in time order.
pub fn words_of(events: &[Event]) -> Vec<Word> {
    let mut words: Vec<Word> = events
        .iter()
        .flat_map(|e| match e {
            Event::Segment { words, .. } => words.clone(),
            _ => Vec::new(),
        })
        .collect();
    words.sort_by(|a, b| a.start.total_cmp(&b.start));
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(start: f64, end: f64, text: &str) -> Word {
        Word { start, end, text: text.into(), probability: 1.0 }
    }

    /// "Hello there, how are you? I'm fine." with a long pause before "Anyway".
    fn speech() -> Vec<Word> {
        vec![
            w(0.0, 0.3, " Hello"),
            w(0.35, 0.6, " there,"),
            w(0.7, 0.9, " how"),
            w(0.95, 1.1, " are"),
            w(1.15, 1.4, " you?"),
            w(1.6, 1.8, " I'm"),
            w(1.85, 2.2, " fine."),
            w(4.0, 4.4, " Anyway"),
        ]
    }

    fn texts(c: &[Caption]) -> Vec<&str> {
        c.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn events_parse_and_log_lines_are_skipped() {
        let line = r#"{"type": "segment", "start": 0.0, "end": 1.0, "text": " Hi", "words": [{"start": 0.0, "end": 0.4, "word": " Hi", "p": 0.9}]}"#;
        let e = Event::parse(line).expect("a segment");
        assert_eq!(words_of(&[e]), vec![Word { start: 0.0, end: 0.4, text: " Hi".into(), probability: 0.9 }]);
        assert_eq!(Event::parse(r#"{"type": "info", "language": "en", "duration": 12.5}"#), Some(Event::Info { language: "en".into(), duration: 12.5 }));
        assert_eq!(Event::parse("Downloading model.bin: 45%|####"), None);
        assert!(matches!(Event::parse(r#"{"type": "error", "message": "no"}"#), Some(Event::Error { .. })));
    }

    #[test]
    fn phrases_break_at_punctuation_and_pauses() {
        let c = group(&speech(), &Grouping::default());
        assert_eq!(texts(&c), vec!["Hello there,", "how are you?", "I'm fine.", "Anyway"]);
        // Short gaps close up; the long pause stays.
        assert_eq!(c[0].end, c[1].start);
        assert_eq!(c[2].end, 2.2, "the 1.8 s pause before 'Anyway' isn't bridged");
        assert_eq!(c[1].words, 2..5);
    }

    #[test]
    fn limits_modes_and_text_options() {
        let words = speech();
        let fill = Grouping { mode: Mode::Words, max_words: 3, ..Default::default() };
        assert_eq!(texts(&group(&words, &fill)), vec!["Hello there, how", "are you? I'm", "fine.", "Anyway"]);
        let sentences = Grouping { mode: Mode::Sentences, max_words: 10, ..Default::default() };
        assert_eq!(texts(&group(&words, &sentences)), vec!["Hello there, how are you?", "I'm fine.", "Anyway"]);
        let single = Grouping { mode: Mode::Single, uppercase: true, strip_punctuation: true, ..Default::default() };
        assert_eq!(texts(&group(&words, &single))[..3], ["HELLO", "THERE", "HOW"]);
        let short = Grouping { mode: Mode::Words, max_words: 99, max_seconds: 1.0, ..Default::default() };
        assert_eq!(texts(&group(&words, &short))[0], "Hello there, how", "'are' would end past a second in");
    }

    /// Splits and joins made by hand hold whatever the rules say, and survive a change
    /// of rules.
    #[test]
    fn hand_made_splits_and_joins() {
        let words = speech();
        let mut manual = Manual::default();
        manual.split(1); // "Hello" | "there,"
        manual.join(2); // "there," + "how are you?"
        let c = group_with(&words, &Grouping::default(), &manual);
        assert_eq!(texts(&c), vec!["Hello", "there, how are you?", "I'm fine.", "Anyway"]);
        let single = group_with(&words, &Grouping { mode: Mode::Single, ..Default::default() }, &manual);
        assert_eq!(texts(&single)[..3], ["Hello", "there, how", "are"], "the join holds in one-word mode");
        manual.split(2);
        assert_eq!(texts(&group_with(&words, &Grouping::default(), &manual))[..3], ["Hello", "there,", "how are you?"]);
    }

    #[test]
    fn short_captions_stay_up_a_moment() {
        let words = vec![w(0.0, 0.1, "Hi"), w(5.0, 5.1, "Bye")];
        let c = group(&words, &Grouping { min_seconds: 0.5, ..Default::default() });
        assert_eq!((c[0].start, c[0].end), (0.0, 0.5));
        assert_eq!((c[1].start, c[1].end), (5.0, 5.5));
        assert!(group(&[], &Grouping::default()).is_empty());
    }
}
