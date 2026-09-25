//! The installed plugins and which of them are on.
//!
//! Atelier Core (every built-in effect, including the sound effects) is always in the
//! list; the rest are folders under `<config>/plugins` (see [`crate::settings`]). The
//! effect registry the whole app renders with is built from the enabled ones, so turning
//! a plugin off really does take its effects out of the menus and out of rendering —
//! clips that still use them keep their settings and are reported as missing.

use oa_graph::plugin::{self, Plugin};
use oa_graph::registry::{EffectKind, Registry};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub struct Plugins {
    /// Where the user's plugins live (also scanned: `./plugins` beside the app, so a
    /// checkout's examples work without installing anything).
    pub dir: PathBuf,
    /// Other folders that were scanned (the checkout's `./plugins`).
    pub extra_dirs: Vec<PathBuf>,
    /// Atelier Core first, then the rest by name.
    pub list: Vec<Plugin>,
    pub disabled: BTreeSet<String>,
    /// Plugins that couldn't be read at all.
    pub errors: Vec<String>,
}

impl Plugins {
    /// Atelier Core plus everything under `dir`.
    pub fn load(dir: PathBuf, disabled: &[String]) -> Self {
        let (mut found, mut errors) = plugin::load_dir(&dir);
        let mut extra_dirs = Vec::new();
        let local = PathBuf::from("plugins");
        if local.is_dir() && local != dir {
            extra_dirs.push(std::fs::canonicalize(&local).unwrap_or(local.clone()));
            let (more, e) = plugin::load_dir(&local);
            for p in more {
                if found.iter().any(|f: &Plugin| f.id == p.id) {
                    errors.push(format!("{}: already installed", p.name));
                } else {
                    found.push(p);
                }
            }
            errors.extend(e);
        }
        found.sort_by_key(|p| p.name.to_lowercase());
        // Sound shaders are checked here, so a broken one is listed with the plugin's
        // other problems (and skipped) like a WGSL effect with a missing file.
        for p in &mut found {
            let mut issues = Vec::new();
            p.effects.retain(|d| {
                if d.kind != EffectKind::Sound {
                    return true;
                }
                match oa_audio::fx::FxInfo::from_descriptor(d) {
                    Ok(_) => true,
                    Err(e) => {
                        issues.push(format!("sound effect \"{}\": {e}", d.type_id));
                        false
                    }
                }
            });
            p.issues.extend(issues);
        }
        let mut list = vec![plugin::core()];
        list.extend(found);
        Plugins { dir, extra_dirs, list, disabled: disabled.iter().cloned().collect(), errors }
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        !self.disabled.contains(id)
    }

    pub fn enabled(&self) -> impl Iterator<Item = &Plugin> {
        self.list.iter().filter(|p| self.is_enabled(&p.id))
    }

    /// The registry to render with: the effects of every enabled plugin. Clashes and
    /// other complaints come back as messages for the user. Also hands the enabled
    /// plugins' sound shaders to the mixer (compiled; errors come back the same way).
    pub fn registry(&self) -> (Registry, Vec<String>) {
        let (registry, mut issues) = Registry::from_plugins(self.enabled());
        let mut shaders = Vec::new();
        for p in self.enabled().filter(|p| !p.builtin) {
            for d in p.sounds() {
                match oa_audio::fx::FxInfo::from_descriptor(d) {
                    Ok(fx) => shaders.push(fx),
                    Err(e) => issues.push(format!("{}: {}: {e}", p.name, d.type_id)),
                }
            }
        }
        issues.extend(oa_audio::fx::install(shaders).into_iter().map(|e| format!("sound effect {e}")));
        (registry, issues)
    }

    /// Sound effects to offer: every enabled plugin's that can actually run.
    pub fn sounds<'a>(&self, registry: &'a Registry) -> Vec<&'a std::sync::Arc<oa_graph::registry::EffectDescriptor>> {
        registry.sounds().into_iter().filter(|d| d.kind == EffectKind::Sound && oa_audio::fx::info(&d.type_id).is_some()).collect()
    }

    /// Effects that clips use but no enabled plugin provides, and who would provide them
    /// (for the missing-effect warning).
    pub fn provider_of(&self, type_id: &str) -> Option<&Plugin> {
        self.list.iter().find(|p| p.effects.iter().any(|d| &*d.type_id == type_id))
    }

    /// Everything that went wrong loading or registering, for the plugins page.
    pub fn issues(&self) -> Vec<String> {
        let mut out = self.errors.clone();
        for p in &self.list {
            out.extend(p.issues.iter().map(|i| format!("{}: {i}", p.name)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_graph::plugin::CORE_ID;
    use oa_graph::registry::EffectUsage;

    /// Building a registry installs sound shaders in the mixer, which is process-wide.
    static MIXER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn empty_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oa-plugins-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// Atelier Core is always listed, provides the built-ins and the sound effects, and
    /// turning it off empties the registry.
    #[test]
    fn core_can_be_turned_off() {
        let _one_at_a_time = MIXER.lock().unwrap_or_else(|e| e.into_inner());
        let dir = empty_dir("core");
        let on = Plugins::load(dir.clone(), &[]);
        assert_eq!(on.list.len(), 1);
        let (registry, issues) = on.registry();
        assert!(issues.is_empty(), "{issues:?}");
        assert!(on.sounds(&registry).iter().any(|d| &*d.type_id == "oa.audio.fade"), "sound effects come with it");
        assert!(registry.effect("oa.blur.gaussian").is_some());
        assert_eq!(registry.plugin_of("oa.blur.gaussian").map(|p| &**p), Some(CORE_ID));
        assert!(on.provider_of("oa.audio.echo").is_some_and(|p| p.id == CORE_ID));

        let off = Plugins::load(dir.clone(), &[CORE_ID.to_string()]);
        let (none, _) = off.registry();
        assert!(none.effects().all(|d| oa_graph::registry::is_internal_effect(&d.type_id)), "only the host's own effects are left with core off");
        assert!(none.offered(EffectUsage::Passive).is_empty() && none.transitions().next().is_none(), "and nothing is offered");
        assert!(off.sounds(&none).is_empty(), "and nothing is heard");
        assert_eq!(off.list.len(), 1, "it's still listed, so it can be turned back on");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The example plugin shipped in `plugins/` loads cleanly: its WGSL registers and its
    /// sound shader compiles.
    #[test]
    fn the_example_plugin_loads() {
        let _one_at_a_time = MIXER.lock().unwrap_or_else(|e| e.into_inner());
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let plugins = Plugins::load(dir, &[]);
        assert!(plugins.issues().is_empty(), "{:?}", plugins.issues());
        let (registry, issues) = plugins.registry();
        assert!(issues.is_empty(), "{issues:?}");
        assert!(registry.effect("com.example.vignette").is_some());
        assert!(plugins.sounds(&registry).iter().any(|d| &*d.type_id == "com.example.telephone"));
        // Its motion intro drops in from above and comes to rest.
        let drop = registry.effect("com.example.drop").expect("a motion effect");
        let values = oa_params::ParamSet::default().eval(&drop.params, None, &oa_params::EvalContext::at(oa_time::Time::ZERO, oa_time::Time::ZERO));
        let at = |visibility: f64| {
            let input = oa_graph::registry::MotionInput { visibility, progress: visibility, seconds: 0.0, canvas: [1920.0, 1080.0], seed: 1, leaving: false };
            drop.motion.as_ref().expect("its script").eval(&values, &input)
        };
        assert!((at(0.0).offset[1] + 540.0).abs() < 1.0, "starts half a canvas up: {:?}", at(0.0));
        assert_eq!((at(1.0).offset, at(1.0).opacity), ([0.0, 0.0], 1.0), "and lands");
        let _ = Plugins::load(empty_dir("reset"), &[]).registry();
    }

    /// A plugin's sound effect reaches the mixer when the plugin is on, and a shader that
    /// doesn't compile is reported rather than offered.
    #[test]
    fn plugin_sound_shaders_are_installed() {
        let _one_at_a_time = MIXER.lock().unwrap_or_else(|e| e.into_inner());
        let dir = empty_dir("sound");
        std::fs::create_dir_all(dir.join("noisy")).expect("dir");
        std::fs::write(
            dir.join("noisy").join("plugin.json"),
            r#"{"id": "com.example.noisy", "name": "Noisy", "effects": [
                {"id": "com.example.half", "name": "Half", "kind": "sound", "shader": {"source": "out = in * 0.5;"}},
                {"id": "com.example.broken", "name": "Broken", "kind": "sound", "shader": {"source": "out = in *;"}}
            ]}"#,
        )
        .expect("manifest");
        let on = Plugins::load(dir.clone(), &[]);
        assert!(on.issues().iter().any(|i| i.contains("com.example.broken") && i.contains("line 1")), "{:?}", on.issues());
        let (registry, issues) = on.registry();
        assert!(issues.is_empty(), "{issues:?}");
        let offered: Vec<&str> = on.sounds(&registry).iter().map(|d| &*d.type_id).collect();
        assert!(offered.contains(&"com.example.half") && !offered.contains(&"com.example.broken"), "{offered:?}");
        assert!(oa_audio::fx::info("com.example.half").is_some());

        let off = Plugins::load(dir.clone(), &["com.example.noisy".to_string()]);
        let _ = off.registry();
        assert!(oa_audio::fx::info("com.example.half").is_none(), "turned off: gone from the mixer too");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
