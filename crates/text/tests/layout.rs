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
    }
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
