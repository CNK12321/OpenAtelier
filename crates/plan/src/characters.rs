//! Text effects that change the characters themselves, not just move or color them.
//!
//! Both are worked out here and drawn by the text pass as **slot swaps**
//! ([`oa_graph::SlotSwap`]): a letter's place keeps where the text puts it (the slots
//! stay steady), and draws other characters in it for now — rolling, flipping or fading
//! from one to the next.
//!
//! * **Characters** — an intro or an outro: each letter starts from another character
//!   (a random one, or none) and settles into its own, one letter after another
//!   (`order`, `stagger`), by a cut, a roll, a flip or a fade, passing through random
//!   characters on the way (`scramble`).
//! * **Counter** — the title shows a number (keyframe it to count), formatted with
//!   `decimals`, a thousands `separator`, a `prefix` and a `suffix`; with `style` roll,
//!   its digits turn like an odometer's.

use oa_doc::{EffectRole, Item};
use oa_graph::registry::{Registry, CHARACTERS, COUNTER};
use oa_graph::{SlotDraw, SlotSwap};
use oa_params::{EvalContext, Evaluated, ParamSchema, Unit, Value};
use std::sync::OnceLock;

/// How far, in ems, a rolling character travels from its slot to out of sight.
const ROLL: f32 = 1.1;

/// The Counter's parameters — the same as its manifest entry (a test holds them to
/// it), here so a title's words can be worked out without a registry.
pub fn counter_schema() -> &'static [ParamSchema] {
    static S: OnceLock<Vec<ParamSchema>> = OnceLock::new();
    S.get_or_init(|| {
        let mut decimals = ParamSchema::new("decimals", Value::Float(0.0), Unit::None).range(0.0, 4.0);
        decimals.step = Some(1.0);
        vec![
            ParamSchema::new("value", Value::Float(0.0), Unit::None),
            decimals,
            ParamSchema::new("separator", Value::Enum("comma".into()), Unit::None).options(&SEPARATORS),
            ParamSchema::new("prefix", Value::Text(String::new()), Unit::None),
            ParamSchema::new("suffix", Value::Text(String::new()), Unit::None),
            ParamSchema::new("style", Value::Enum("count".into()), Unit::None).options(&["count", "roll"]),
        ]
    })
}

/// The Counter's separator options: the thousands mark (a period means a decimal comma).
pub const SEPARATORS: [&str; 4] = ["comma", "period", "space", "none"];

/// The Counter running on `item` at `ctx`, if any: its values.
fn counter(item: &Item, ctx: &EvalContext) -> Option<Evaluated> {
    let fx = item
        .active_effects()
        .iter()
        .rev()
        .find(|e| e.type_id == COUNTER && item.effect_on(e) && e.on_duplicate.is_none() && e.role.clock(ctx.clip_time, item.range.duration).is_some())
        .cloned()?;
    Some(fx.params.eval(counter_schema(), None, ctx))
}

/// `x` written out: `decimals` places (rounded, or cut off when `floor` — a rolling
/// counter shows the digit it's turning from), its thousands marked by `separator`.
pub fn format_number(x: f64, decimals: usize, separator: &str, floor: bool) -> String {
    let decimals = decimals.min(4);
    let k = 10f64.powi(decimals as i32);
    let scaled = (x.abs() * k).min(1e15);
    let n = if floor { scaled.floor() } else { scaled.round() } as u64;
    let (thousands, point) = match separator {
        "period" => (".", ","),
        "space" => (" ", "."),
        "none" => ("", "."),
        _ => (",", "."),
    };
    let whole = (n / k as u64).to_string();
    let mut s = String::new();
    if x < 0.0 && n > 0 {
        s.push('-');
    }
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            s.push_str(thousands);
        }
        s.push(c);
    }
    if decimals > 0 {
        s.push_str(point);
        s.push_str(&format!("{:0width$}", n % k as u64, width = decimals));
    }
    s
}

/// What a title with a Counter says instead of its words (`None`: no Counter).
pub fn counter_text(item: &Item, ctx: &EvalContext) -> Option<String> {
    let v = counter(item, ctx)?;
    let text = |id: &str| v.get(id).and_then(Value::as_text).unwrap_or_default().to_string();
    let roll = v.get("style").and_then(Value::as_enum) == Some("roll");
    let sep = v.get("separator").and_then(Value::as_enum).unwrap_or("comma");
    let number = format_number(v.float("value"), v.float("decimals").round().max(0.0) as usize, sep, roll);
    Some(format!("{}{number}{}", text("prefix"), text("suffix")))
}

/// A rolling Counter's turning digits: for each digit (by its letter index in
/// `content`), how far it has turned towards the next, 0..1. The lowest digit turns
/// with the number's fraction; each digit above turns while the one below goes from
/// 9 to 0 (an odometer).
fn rolling_digits(item: &Item, ctx: &EvalContext, content: &str) -> Vec<(u32, u32, f32)> {
    let Some(v) = counter(item, ctx) else { return Vec::new() };
    if v.get("style").and_then(Value::as_enum) != Some("roll") {
        return Vec::new();
    }
    let decimals = v.float("decimals").round().clamp(0.0, 4.0) as i32;
    let scaled = (v.float("value").abs() * 10f64.powi(decimals)).min(1e15);
    let mut turn = scaled - scaled.floor();
    // The number's letters (between the prefix's and the suffix's), with their indices.
    let letters: Vec<(usize, char)> = content.chars().filter(|c| !c.is_whitespace()).enumerate().collect();
    let count = |id: &str| v.get(id).and_then(Value::as_text).unwrap_or_default().chars().filter(|c| !c.is_whitespace()).count();
    let start = count("prefix").min(letters.len());
    let end = letters.len().saturating_sub(count("suffix")).max(start);
    let mut out = Vec::new();
    // From the lowest place up.
    for &(index, c) in letters[start..end].iter().rev() {
        let Some(digit) = c.to_digit(10) else { continue };
        if turn > 1e-3 {
            out.push((index as u32, (digit + 1) % 10, turn as f32));
        }
        // The next place up turns only while this one goes from 9 to 0.
        if digit != 9 {
            turn = 0.0;
        }
    }
    out
}

/// A character set for random characters (`characters`), picked to match the case of
/// the letter it stands in for.
fn pool(set: &str, target: char) -> &'static [u8] {
    let lower = target.is_lowercase();
    match set {
        "letters" if lower => b"abcdefghijklmnopqrstuvwxyz",
        "letters" => b"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "digits" => b"0123456789",
        "symbols" => b"!#$%&*+=?<>/\\|~^@",
        "binary" => b"01",
        _ if lower => b"abcdefghijklmnopqrstuvwxyz0123456789",
        _ => b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
    }
}

fn hash(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn hash3(a: u64, b: u64, c: u64) -> u64 {
    hash(a ^ hash(b ^ hash(c)))
}

/// What one step of a letter's change shows.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Shown {
    Blank,
    /// The letter's own character.
    Own,
    Char(char),
}

impl Shown {
    fn draw(self, dy: f32, scale_y: f32, alpha: f32) -> Option<SlotDraw> {
        let ch = match self {
            Shown::Blank => return None,
            Shown::Own => None,
            Shown::Char(c) => Some(c),
        };
        Some(SlotDraw { ch, dy, scale_y, alpha })
    }
}

/// Each letter's rank in the order it settles (0 first), for `count` letters in
/// `words` (each letter's word).
fn ranks(order: &str, words: &[u32], seed: u64) -> Vec<f64> {
    let n = words.len();
    let mid = (n as f64 - 1.0) / 2.0;
    (0..n)
        .map(|i| match order {
            "right to left" => (n - 1 - i) as f64,
            "random" => (hash3(i as u64, seed, 7) % 10_000) as f64,
            "center out" => (i as f64 - mid).abs(),
            "by word" => words[i] as f64,
            _ => i as f64,
        })
        .collect()
}

/// One letter's change at its own progress `q` (0: still showing `from`, 1: its own):
/// what its slot draws, and whether it's clipped to the slot.
fn letter(style: &str, q: f64, from: Shown, scramble: f64, random: &dyn Fn(u64) -> Shown, flicker: Shown) -> (Vec<SlotDraw>, bool) {
    if q >= 1.0 {
        return (vec![SlotDraw { ch: None, dy: 0.0, scale_y: 1.0, alpha: 1.0 }], false);
    }
    if q <= 0.0 {
        return (from.draw(0.0, 1.0, 1.0).into_iter().collect(), false);
    }
    // The steps from the old character to the new: the old, some random ones, the new.
    let mut steps = vec![from];
    steps.extend((0..(scramble * 8.0).round() as u64).map(random));
    steps.push(Shown::Own);
    let pos = q * (steps.len() - 1) as f64;
    let i = (pos.floor() as usize).min(steps.len() - 2);
    let f = (pos - i as f64).clamp(0.0, 1.0) as f32;
    let (a, b) = (steps[i], steps[i + 1]);
    match style {
        "roll" => {
            // Smooth in and out of each step, like a reel.
            let f = f * f * (3.0 - 2.0 * f);
            (a.draw(-f * ROLL, 1.0, 1.0).into_iter().chain(b.draw((1.0 - f) * ROLL, 1.0, 1.0)).collect(), true)
        }
        "flip" => {
            // A flap folding down: the old one closes, then the new one opens.
            let d = if f < 0.5 { a.draw(0.0, 1.0 - 2.0 * f, 1.0) } else { b.draw(0.0, 2.0 * f - 1.0, 1.0) };
            (d.into_iter().collect(), false)
        }
        "fade" => (a.draw(0.0, 1.0, 1.0 - f).into_iter().chain(b.draw(0.0, 1.0, f)).collect(), false),
        // Cut: random characters flicker through (as many as `scramble` asks), then it
        // lands; with none, it switches half-way.
        _ => {
            let shown = if scramble > 0.0 && q < 1.0 - 0.15 * (1.0 - scramble) { flicker } else if q < 0.5 { from } else { Shown::Own };
            (shown.draw(0.0, 1.0, 1.0).into_iter().collect(), false)
        }
    }
}

/// The letters of a title showing other characters at `ctx` (its Characters and a
/// rolling Counter), for `content`, the words it shows.
pub fn slots(item: &Item, registry: &Registry, ctx: &EvalContext, content: &str) -> Vec<SlotSwap> {
    let mut out: Vec<SlotSwap> = Vec::new();
    for (index, next, turn) in rolling_digits(item, ctx, content) {
        let own = Shown::Own.draw(-turn * ROLL, 1.0, 1.0);
        let next = Shown::Char(char::from_digit(next, 10).unwrap_or('0')).draw((1.0 - turn) * ROLL, 1.0, 1.0);
        out.push(SlotSwap { index, clip: true, draws: own.into_iter().chain(next).collect() });
    }
    let Some(d) = registry.effect(CHARACTERS) else { return out };
    // Each letter (whitespace isn't one), and its word.
    let mut letters = Vec::new();
    let mut words = Vec::new();
    let (mut word, mut gap) = (0u32, false);
    for c in content.chars() {
        if c.is_whitespace() {
            gap = true;
            continue;
        }
        if gap && !letters.is_empty() {
            word += 1;
        }
        gap = false;
        letters.push(c);
        words.push(word);
    }
    for fx in item.active_effects().iter().filter(|e| e.type_id == CHARACTERS && item.effect_on(e) && e.on_duplicate.is_none()) {
        // An intro or an outro: the text is all there at full visibility.
        let Some(clock) = fx.role.clock(ctx.clip_time, item.range.duration).filter(|_| fx.role != EffectRole::Passive) else { continue };
        let p = clock.visibility;
        let v = fx.params.eval(&d.params, None, ctx);
        let text = |id: &str| v.get(id).and_then(Value::as_enum).unwrap_or_default().to_string();
        let from = text("from");
        let seed = v.float("seed").round() as u64;
        let scramble = v.float("scramble").clamp(0.0, 1.0);
        let step = (ctx.clip_time.as_seconds_f64() * v.float("rate").max(0.0)).floor() as u64;
        let stagger = v.float("stagger").clamp(0.0, 0.95);
        let ranks = ranks(&text("order"), &words, seed);
        let top = ranks.iter().cloned().fold(0.0, f64::max);
        let style = text("style");
        let set = text("characters");
        for (i, &c) in letters.iter().enumerate() {
            let pick = |a: u64, b: u64| {
                let chars = pool(&set, c);
                Shown::Char(chars[(hash3(i as u64, a, b ^ seed.wrapping_mul(31)) % chars.len() as u64) as usize] as char)
            };
            let start = if from == "blank" { Shown::Blank } else { pick(step, 1) };
            let r = if top > 0.0 { ranks[i] / top } else { 0.0 };
            let q = ((p - stagger * r) / (1.0 - stagger)).clamp(0.0, 1.0);
            let random = |k: u64| pick(k, 2);
            let (draws, clip) = letter(&style, q, start, scramble, &random, pick(step, 3));
            if q >= 1.0 {
                continue;
            }
            out.retain(|s| s.index != i as u32);
            out.push(SlotSwap { index: i as u32, clip, draws });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_with_separators_and_decimals() {
        assert_eq!(format_number(1234567.891, 2, "comma", false), "1,234,567.89");
        assert_eq!(format_number(1234567.891, 1, "period", false), "1.234.567,9");
        assert_eq!(format_number(1234.5, 0, "space", false), "1 235");
        assert_eq!(format_number(1234.5, 0, "space", true), "1 234");
        assert_eq!(format_number(999.0, 0, "none", false), "999");
        assert_eq!(format_number(-42.25, 1, "comma", false), "-42.3");
        assert_eq!(format_number(-0.01, 0, "comma", false), "0");
        assert_eq!(format_number(7.0, 3, "comma", false), "7.000");
    }

    #[test]
    fn a_letter_rolls_from_the_old_character_through_random_ones_to_its_own() {
        let random = |k: u64| Shown::Char((b'a' + k as u8) as char);
        // Before it starts: the old one, still. Done: its own.
        let (d, _) = letter("roll", 0.0, Shown::Char('X'), 0.5, &random, Shown::Blank);
        assert_eq!(d, vec![SlotDraw { ch: Some('X'), dy: 0.0, scale_y: 1.0, alpha: 1.0 }]);
        let (d, _) = letter("roll", 1.0, Shown::Char('X'), 0.5, &random, Shown::Blank);
        assert_eq!(d, vec![SlotDraw { ch: None, dy: 0.0, scale_y: 1.0, alpha: 1.0 }]);
        // Half-way through the first step (of 5: X, a, b, c, d, own), the old one is
        // half up and the next half in, clipped to the slot.
        let (d, clip) = letter("roll", 0.1, Shown::Char('X'), 0.5, &random, Shown::Blank);
        assert!(clip);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].ch, Some('X'));
        assert_eq!(d[1].ch, Some('a'));
        assert!((d[0].dy + 0.5 * ROLL).abs() < 1e-4 && (d[1].dy - 0.5 * ROLL).abs() < 1e-4, "{d:?}");
        // From a blank: nothing to roll away.
        let (d, _) = letter("roll", 0.5, Shown::Blank, 0.0, &random, Shown::Blank);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].ch, None);
    }

    #[test]
    fn a_flap_folds_closed_then_opens_on_the_new_character() {
        let random = |_| Shown::Blank;
        let (d, _) = letter("flip", 0.25, Shown::Char('A'), 0.0, &random, Shown::Blank);
        assert_eq!(d, vec![SlotDraw { ch: Some('A'), dy: 0.0, scale_y: 0.5, alpha: 1.0 }]);
        let (d, _) = letter("flip", 0.75, Shown::Char('A'), 0.0, &random, Shown::Blank);
        assert_eq!(d, vec![SlotDraw { ch: None, dy: 0.0, scale_y: 0.5, alpha: 1.0 }]);
    }

    #[test]
    fn letters_settle_in_the_order_asked() {
        let words = [0, 0, 0, 1, 1];
        assert_eq!(ranks("left to right", &words, 1), vec![0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ranks("right to left", &words, 1), vec![4.0, 3.0, 2.0, 1.0, 0.0]);
        assert_eq!(ranks("center out", &words, 1), vec![2.0, 1.0, 0.0, 1.0, 2.0]);
        assert_eq!(ranks("by word", &words, 1), vec![0.0, 0.0, 0.0, 1.0, 1.0]);
        let random = ranks("random", &words, 1);
        assert_eq!(random, ranks("random", &words, 1));
        assert_ne!(random, ranks("random", &words, 2));
    }
}
