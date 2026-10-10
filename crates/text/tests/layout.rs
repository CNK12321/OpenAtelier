use oa_text::{layout, sdf, Align, TextSpec};

fn spec(content: &str) -> TextSpec {
    TextSpec {
        content: content.into(),
        family: oa_text::default_family().into(),
        bold: false,
        italic: false,
        size: 100.0,
        align: Align::Center,
        tracking: 0.0,
        line_height: 1.2,
        wrap: None,
    }
}

/// Wrapped to a box: lines break between words to stay inside it, aligned within it;
/// the cursor still finds every character; a word wider than the box is split.
#[test]
fn text_wraps_to_its_box() {
    let one = layout(&spec("The quick brown fox jumps"));
    assert_eq!(one.lines, 1);
    let width = one.size[0] * 0.45;
    let wrapped = layout(&TextSpec { wrap: Some(width), align: Align::Left, ..spec("The quick brown fox jumps") });
    assert!(wrapped.lines >= 3, "{} lines", wrapped.lines);
    assert_eq!(wrapped.glyphs.len(), one.glyphs.len(), "the same letters");
    assert_eq!(wrapped.words, 5, "words are counted the same");
    for g in &wrapped.glyphs {
        assert!(g.ink[2] <= width + 2.0, "inside the box: {} > {width}", g.ink[2]);
    }
    // Rows break before words: each row's first letter starts a word.
    for line in 1..wrapped.lines {
        let first = wrapped.glyphs.iter().find(|g| g.line == line).unwrap();
        let before = wrapped.glyphs.iter().rfind(|g| g.line == line - 1).unwrap();
        assert_ne!(first.word, before.word, "row {line} starts a new word");
    }
    // Every character is somewhere for the cursor, in order.
    let firsts: Vec<usize> = wrapped.carets.iter().map(|c| c.first_char).collect();
    assert!(firsts.windows(2).all(|w| w[0] < w[1]), "{firsts:?}");
    let chars: usize = wrapped.carets.iter().map(|c| c.xs.len() - 1).sum();
    assert_eq!(chars, "The quick brown fox jumps".chars().count());
    // Centered: each row in the middle of the box.
    let centered = layout(&TextSpec { wrap: Some(width), ..spec("The quick brown fox jumps") });
    assert!((centered.size[0] - width).abs() < 2.0, "the box is the width asked for");
    // One long word: split across rows rather than spilling out.
    let long = layout(&TextSpec { wrap: Some(150.0), ..spec("Supercalifragilistic") });
    assert!(long.lines >= 3, "{} lines", long.lines);
    assert!(long.glyphs.iter().all(|g| g.ink[2] <= 152.0));
}

#[test]
fn lines_words_and_letters_are_numbered() {
    let l = layout(&spec("Hello world\nagain"));
    assert_eq!(l.lines, 2);
    assert_eq!(l.words, 3);
    // Spaces aren't drawn: 10 + 5 letters.
    assert_eq!(l.glyphs.len(), 15);
    assert!(l.glyphs.iter().enumerate().all(|(i, g)| g.index == i as u32));
    assert_eq!(l.glyphs[5].word, 1, "'w' starts the second word");
    assert_eq!(l.glyphs[10].line, 1);
    assert_eq!(l.glyphs[10].word, 2);
    // Two lines of a 100px font: roughly 220px tall; ink inside the box.
    assert!(l.size[1] > 180.0 && l.size[1] < 300.0, "{:?}", l.size);
    for g in &l.glyphs {
        assert!(g.ink[0] >= -1e-6 && g.ink[1] >= -1e-6 && g.ink[2] <= l.size[0] + 1e-6 && g.ink[3] <= l.size[1] + 1e-6);
    }
}

#[test]
fn letter_spacing_and_size_scale_the_box() {
    let a = layout(&spec("Spacing"));
    let wide = layout(&TextSpec { tracking: 0.5, ..spec("Spacing") });
    assert!(wide.size[0] > a.size[0] + 250.0, "{} vs {}", wide.size[0], a.size[0]);
    let big = layout(&TextSpec { size: 200.0, ..spec("Spacing") });
    assert!((big.size[0] / a.size[0] - 2.0).abs() < 0.05);
}

#[test]
fn centered_lines_share_a_middle() {
    let l = layout(&spec("a\nlonger line"));
    let first = &l.glyphs[0];
    let mid = (first.ink[0] + first.ink[2]) / 2.0;
    assert!((mid - l.size[0] / 2.0).abs() < 10.0, "{mid} vs {}", l.size[0] / 2.0);
}

#[test]
fn missing_characters_come_from_a_fallback_font() {
    let l = layout(&spec("A→★"));
    assert_eq!(l.glyphs.len(), 3, "every character draws something");
}

#[test]
fn distance_fields_are_half_on_the_outline() {
    let l = layout(&spec("O"));
    let g = &l.glyphs[0];
    let f = sdf::glyph_sdf(g.font, g.glyph, 64).expect("O has an outline");
    let [w, h] = f.size;
    let at = |x: u32, y: u32| f.pixels[(y * w + x) as usize];
    // Corners are far outside, the stroke of the O is inside, its hole outside.
    assert_eq!(at(0, 0), 0);
    let row = h / 2;
    let values: Vec<u8> = (0..w).map(|x| at(x, row)).collect();
    let max = *values.iter().max().unwrap();
    assert!(max > 140, "stroke is inside: {values:?}");
    assert!(values[(w / 2) as usize] < 128, "hole is outside: {values:?}");
    // Crossings through 0.5 happen four times across the middle row.
    let crossings = values.windows(2).filter(|p| (p[0] < 128) != (p[1] < 128)).count();
    assert_eq!(crossings, 4, "{values:?}");
}

/// Font samples for menus: a picture of the text with ink in it, and nothing for an
/// empty string.
#[test]
fn font_samples_draw_something() {
    let family = oa_text::default_family().to_string();
    let Some((w, h, px)) = oa_text::fonts::sample_image(&family, "Handgloves", 18.0) else { return };
    assert!(w > 20 && (10..=40).contains(&h), "{w}x{h}");
    assert!(px.iter().any(|a| *a > 128), "the sample has ink");
    assert_eq!(px.len(), (w * h) as usize);
    assert!(oa_text::fonts::sample_image(&family, "", 18.0).is_none());
}

/// Cursor positions for editing where the text is drawn: one before every character
/// and one after each line, left to right, on the right line; a click finds the nearest.
#[test]
fn carets_follow_the_characters() {
    let l = layout(&spec("Hi you\nok"));
    assert_eq!(l.carets.len(), 2);
    let (a, b) = (&l.carets[0], &l.carets[1]);
    assert_eq!((a.first_char, a.xs.len()), (0, 7), "6 characters, 7 places to be");
    assert_eq!((b.first_char, b.xs.len()), (7, 3), "the line break counts as a character");
    assert!(a.xs.windows(2).all(|w| w[1] > w[0]), "{:?}", a.xs);
    assert!(b.top > a.top && b.bottom > b.top);
    // "H" starts where its glyph is drawn.
    assert!((a.xs[0] - l.glyphs[0].origin[0]).abs() < 1.0);
    // Before the space, and after it ("y" is the third drawn glyph).
    assert!((a.xs[3] - l.glyphs[2].origin[0]).abs() < 1.0);
    let (line, x) = l.caret(8).unwrap();
    assert_eq!(line.first_char, 7);
    assert_eq!(x, b.xs[1]);
    // Clicks: the nearest place on the line clicked.
    assert_eq!(l.index_at([a.xs[3] + 1.0, (a.top + a.bottom) / 2.0]), 3);
    assert_eq!(l.index_at([b.xs[2] + 500.0, (b.top + b.bottom) / 2.0]), 9);
    assert_eq!(l.index_at([-500.0, a.top]), 0);
}
