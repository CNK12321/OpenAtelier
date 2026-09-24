//! Scripted input for testing the UI without touching the real mouse:
//!
//!   oa-app samples/* --script steps.txt
//!
//! One command per line (`#` comments), coordinates in window pixels:
//!
//! ```text
//! wait 30                 # frames
//! click 700 400
//! rclick 700 400          # right-click (context menus)
//! drag 1080 610 900 500   # press, move in steps, release
//! mods shift              # held for what follows (shift, ctrl, alt, none)
//! key S                   # an egui key name: S, Delete, ArrowRight, Z, …
//! type Hello world        # text typed into the focused field
//! wheel 0 -120            # mouse wheel at the pointer (lines × 40 px); with `mods ctrl`, zooms
//! print                   # log the selection and its transform to stderr
//! ```
//!
//! Events are injected into egui's raw input one step per frame, so they go through
//! exactly the same paths as real input.

use eframe::egui;
use std::collections::VecDeque;

#[derive(Debug)]
enum Step {
    Wait(u32),
    Move([f32; 2]),
    Button(bool),
    RightButton(bool),
    Key(egui::Key),
    Mods(egui::Modifiers),
    Wheel([f32; 2]),
    Text(String),
    Print,
}

pub struct Script {
    steps: VecDeque<Step>,
    pointer: Option<egui::Pos2>,
    mods: egui::Modifiers,
    pub finished: bool,
}

impl Script {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut steps = VecDeque::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            let num = |i: usize| -> Result<f32, String> {
                words.get(i).and_then(|w| w.parse().ok()).ok_or(format!("line {}: expected a number", n + 1))
            };
            match words[0] {
                "wait" => steps.push_back(Step::Wait(num(1)? as u32)),
                "move" => steps.push_back(Step::Move([num(1)?, num(2)?])),
                "rclick" => {
                    steps.push_back(Step::Move([num(1)?, num(2)?]));
                    steps.push_back(Step::RightButton(true));
                    steps.push_back(Step::RightButton(false));
                    steps.push_back(Step::Wait(20));
                }
                "click" => {
                    steps.push_back(Step::Move([num(1)?, num(2)?]));
                    steps.push_back(Step::Button(true));
                    steps.push_back(Step::Button(false));
                    // Human pace: nothing follows a click within a third of a second, so
                    // the next press isn't taken as part of a double-click.
                    steps.push_back(Step::Wait(20));
                }
                "drag" => {
                    let (a, b) = ([num(1)?, num(2)?], [num(3)?, num(4)?]);
                    let n = words.get(5).and_then(|w| w.parse().ok()).unwrap_or(12u32);
                    steps.push_back(Step::Move(a));
                    steps.push_back(Step::Button(true));
                    for i in 1..=n {
                        let f = i as f32 / n as f32;
                        steps.push_back(Step::Move([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]));
                    }
                    steps.push_back(Step::Button(false));
                    steps.push_back(Step::Wait(20));
                }
                "key" => {
                    let key = words.get(1).and_then(|k| egui::Key::from_name(k)).ok_or(format!("line {}: unknown key", n + 1))?;
                    steps.push_back(Step::Key(key));
                    steps.push_back(Step::Wait(1));
                }
                "mods" => {
                    let mut m = egui::Modifiers::NONE;
                    for w in &words[1..] {
                        match *w {
                            "shift" => m.shift = true,
                            "ctrl" => {
                                m.ctrl = true;
                                m.command = true;
                            }
                            "alt" => m.alt = true,
                            _ => {}
                        }
                    }
                    steps.push_back(Step::Mods(m));
                }
                "type" => {
                    steps.push_back(Step::Text(line[4..].trim().to_string()));
                    steps.push_back(Step::Wait(1));
                }
                "print" => steps.push_back(Step::Print),
                "wheel" => {
                    steps.push_back(Step::Wheel([num(1)?, num(2)?]));
                    steps.push_back(Step::Wait(3));
                }
                other => return Err(format!("line {}: unknown command {other:?}", n + 1)),
            }
        }
        Ok(Script { steps, pointer: None, mods: egui::Modifiers::NONE, finished: false })
    }

    /// Feeds this frame's step into `input`. Returns true when the app should print its
    /// selection state.
    pub fn feed(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) -> bool {
        ctx.request_repaint();
        // Keep the pointer "in the window" every frame, as a real mouse would be.
        if let Some(p) = self.pointer {
            input.events.push(egui::Event::PointerMoved(p));
        }
        let Some(step) = self.steps.pop_front() else {
            self.finished = true;
            return false;
        };
        let ppp = ctx.pixels_per_point();
        match step {
            Step::Wait(n) => {
                if n > 1 {
                    self.steps.push_front(Step::Wait(n - 1));
                }
            }
            Step::Move([x, y]) => {
                let p = egui::pos2(x / ppp, y / ppp);
                self.pointer = Some(p);
                input.events.push(egui::Event::PointerMoved(p));
            }
            Step::RightButton(pressed) => {
                if let Some(pos) = self.pointer {
                    input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: self.mods });
                }
            }
            Step::Button(pressed) => {
                if let Some(pos) = self.pointer {
                    input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: self.mods });
                }
            }
            Step::Key(key) => {
                for pressed in [true, false] {
                    input.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: self.mods });
                }
            }
            Step::Mods(m) => {
                self.mods = m;
                input.events.push(egui::Event::ModifiersChanged(m));
            }
            Step::Wheel(delta) => input.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(delta[0], delta[1]),
                phase: egui::TouchPhase::Move,
                modifiers: self.mods,
            }),
            Step::Text(s) => input.events.push(egui::Event::Text(s)),
            Step::Print => return true,
        }
        false
    }
}
