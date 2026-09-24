//! Plugins: bundles of effects the host can turn on and off.
//!
//! Everything the editor can do to a picture comes from a plugin. The built-ins ship
//! inside the binary as **Atelier Core** ([`core`]), and further plugins are folders
//! under the host's plugins directory, each with a `plugin.json` naming its effects and
//! their WGSL. Nothing else is loaded — there's no code execution here, only shaders
//! going through the same [`EffectDescriptor`] path built-ins use, so a plugin can't do
//! anything a built-in effect couldn't.
//!
//! ```text
//! plugins/
//!   example-looks/
//!     plugin.json
//!     vignette.wgsl
//! ```
//!
//! Ids are namespaced: `oa.*` is reserved for Atelier Core, so a plugin's effects must
//! use their own prefix (`com.example.vignette`). Problems — bad JSON, a missing shader
//! file, a reserved id — are collected in [`Plugin::issues`] and shown to the user
//! rather than failing the load.

use crate::registry::{builtins, EffectDescriptor, EffectKind, EffectShader, EffectUsage, WorkingSpace, PLUGIN_API_VERSION};
use oa_params::{Gradient, ParamId, ParamSchema, Unit, Value};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Atelier Core's id. It's built in, always present and on by default.
pub const CORE_ID: &str = "com.openatelier.core";

/// The manifest file inside a plugin folder.
pub const MANIFEST: &str = "plugin.json";

/// A bundle of effects, loaded or built in.
#[derive(Clone, Debug)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    /// Shipped inside the binary (Atelier Core): it can't be removed, only turned off.
    pub builtin: bool,
    /// Where it was loaded from.
    pub path: Option<PathBuf>,
    /// Picture, text and sound effects ([`EffectKind::Sound`]) alike.
    pub effects: Vec<EffectDescriptor>,
    /// What was wrong with it (skipped effects, unreadable shaders…).
    pub issues: Vec<String>,
}

impl Plugin {
    /// Picture and text effects a user can pick from this plugin.
    pub fn offered_effects(&self) -> usize {
        self.effects.iter().filter(|d| d.kind != EffectKind::Sound && !crate::registry::is_internal_effect(&d.type_id)).count()
    }

    /// Its sound effects.
    pub fn sounds(&self) -> impl Iterator<Item = &EffectDescriptor> {
        self.effects.iter().filter(|d| d.kind == EffectKind::Sound)
    }

    /// One line for the plugin list: what it provides.
    pub fn summary(&self) -> String {
        let fx = self.offered_effects();
        let mut parts = vec![format!("{fx} effect{}", if fx == 1 { "" } else { "s" })];
        let sounds = self.sounds().count();
        if sounds > 0 {
            parts.push(format!("{sounds} sound effect{}", if sounds == 1 { "" } else { "s" }));
        }
        parts.join(" · ")
    }
}

/// Atelier Core: every built-in effect. `sounds` are the host's sound effects (the host
/// passes them in, since the processing lives in `oa-audio`), as [`EffectKind::Sound`]
/// descriptors.
pub fn core(sounds: Vec<EffectDescriptor>) -> Plugin {
    let mut effects = builtins();
    effects.extend(sounds);
    Plugin {
        id: CORE_ID.into(),
        name: "Atelier Core".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "The effects OpenAtelier ships with: color, blur, keying, motion, transitions, text animation and sound.".into(),
        author: "OpenAtelier".into(),
        builtin: true,
        path: None,
        effects,
        issues: Vec::new(),
    }
}

// ---- manifests -------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    id: String,
    name: String,
    #[serde(default = "unknown_version")]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    /// The plugin API this was written against; a different one is refused.
    #[serde(default = "default_api")]
    api_version: u32,
    #[serde(default)]
    effects: Vec<EffectDef>,
}

fn unknown_version() -> String {
    "0".into()
}

fn default_api() -> u32 {
    PLUGIN_API_VERSION
}

#[derive(Debug, Deserialize, Serialize)]
struct EffectDef {
    id: String,
    name: String,
    /// point · uv_warp · spatial · transition · glyph · glyph_pixel · sound
    kind: String,
    /// passive (default) · in_out · cut
    #[serde(default)]
    usage: Option<String>,
    /// linear (default) · display
    #[serde(default)]
    space: Option<String>,
    /// For `spatial` and `glyph`: the parameter the output grows by.
    #[serde(default)]
    expand: Option<String>,
    /// It animates on its own (reads `progress()` / `clip_seconds()`).
    #[serde(default)]
    time_varying: bool,
    /// Opaque input stays opaque. Default: true for `point`, false otherwise.
    #[serde(default)]
    preserves_opacity: Option<bool>,
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    params: Vec<ParamDef>,
    shader: ShaderDef,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Deserialize, Serialize)]
struct ShaderDef {
    /// The WGSL function's name (not used by sound shaders, which are one program).
    #[serde(default)]
    entry: String,
    #[serde(default = "one")]
    passes: u32,
    /// The WGSL itself, or `file` naming a `.wgsl` beside the manifest.
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    file: Option<String>,
}

/// A parameter, in the friendly form manifests are written in:
/// `{"id": "amount", "type": "float", "default": 0.5, "min": 0, "max": 1}`.
#[derive(Debug, Deserialize, Serialize)]
struct ParamDef {
    id: String,
    /// float · int · bool · vec2 · vec3 · color · enum · gradient · media · text
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    /// none (default) · layer_pixels · canvas_fraction · source_fraction · degrees ·
    /// direction · seconds · decibels
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    options: Vec<String>,
    /// The value can't be keyframed (a mode the shader compiles against).
    #[serde(default)]
    static_only: bool,
}

fn unit_of(name: Option<&str>) -> Result<Unit, String> {
    Ok(match name.unwrap_or("none") {
        "none" => Unit::None,
        "layer_pixels" => Unit::LayerPixels,
        "canvas_fraction" => Unit::CanvasFraction,
        "source_fraction" => Unit::SourceFraction,
        "degrees" => Unit::Degrees,
        "direction" => Unit::Direction,
        "seconds" => Unit::Seconds,
        "decibels" => Unit::Decibels,
        other => return Err(format!("unknown unit \"{other}\"")),
    })
}

fn number(v: Option<&serde_json::Value>, fallback: f64) -> f64 {
    v.and_then(|v| v.as_f64()).unwrap_or(fallback)
}

fn numbers<const N: usize>(v: Option<&serde_json::Value>, fallback: [f64; N]) -> [f64; N] {
    let mut out = fallback;
    if let Some(list) = v.and_then(|v| v.as_array()) {
        for (i, x) in list.iter().take(N).enumerate() {
            out[i] = x.as_f64().unwrap_or(out[i]);
        }
    }
    out
}

impl ParamDef {
    fn into_schema(self) -> Result<ParamSchema, String> {
        let d = self.default.as_ref();
        let default = match self.ty.as_str() {
            "float" => Value::Float(number(d, 0.0)),
            "int" => Value::Int(d.and_then(|v| v.as_i64()).unwrap_or(0)),
            "bool" => Value::Bool(d.and_then(|v| v.as_bool()).unwrap_or(false)),
            "vec2" => Value::Vec2(numbers(d, [0.0, 0.0])),
            "vec3" => Value::Vec3(numbers(d, [0.0, 0.0, 0.0])),
            "color" => Value::Color(numbers(d, [1.0, 1.0, 1.0, 1.0])),
            "gradient" => Value::Gradient(Gradient::solid(numbers(d, [1.0, 1.0, 1.0, 1.0]))),
            "media" => Value::Media(None),
            "text" => Value::Text(d.and_then(|v| v.as_str()).unwrap_or_default().into()),
            "enum" => {
                let first = self.options.first().ok_or_else(|| format!("enum parameter \"{}\" has no options", self.id))?;
                Value::Enum(d.and_then(|v| v.as_str()).unwrap_or(first).into())
            }
            other => return Err(format!("parameter \"{}\": unknown type \"{other}\"", self.id)),
        };
        let mut schema = ParamSchema::new(&self.id, default, unit_of(self.unit.as_deref()).map_err(|e| format!("parameter \"{}\": {e}", self.id))?);
        if let (Some(lo), Some(hi)) = (self.min, self.max) {
            schema.range = Some((lo, hi));
        }
        schema.options = self.options;
        schema.animatable = !self.static_only;
        Ok(schema)
    }
}

impl EffectDef {
    fn into_descriptor(self, dir: Option<&Path>) -> Result<EffectDescriptor, String> {
        let where_ = format!("effect \"{}\"", self.id);
        if self.id.starts_with("oa.") {
            return Err(format!("{where_}: ids starting with \"oa.\" are reserved for Atelier Core"));
        }
        let expand = self.expand.as_deref().map(ParamId::new);
        let kind = match self.kind.as_str() {
            "point" => EffectKind::PointOp,
            "uv_warp" => EffectKind::UvWarp,
            "spatial" => EffectKind::Spatial { expand },
            "transition" => EffectKind::Transition,
            "glyph" => EffectKind::Glyph { expand },
            "glyph_pixel" => EffectKind::GlyphPixel,
            "sound" => EffectKind::Sound,
            other => return Err(format!("{where_}: unknown kind \"{other}\"")),
        };
        let usage = match self.usage.as_deref() {
            None => {
                if kind == EffectKind::Transition {
                    EffectUsage::Cut
                } else {
                    EffectUsage::Passive
                }
            }
            Some("passive") => EffectUsage::Passive,
            Some("in_out") => EffectUsage::InOut,
            Some("cut") => EffectUsage::Cut,
            Some(other) => return Err(format!("{where_}: unknown usage \"{other}\"")),
        };
        let space = match self.space.as_deref() {
            None | Some("linear") => WorkingSpace::Linear,
            Some("display") => WorkingSpace::Display,
            Some(other) => return Err(format!("{where_}: unknown space \"{other}\"")),
        };
        let source = match (&self.shader.source, &self.shader.file) {
            (Some(s), _) => s.clone(),
            (None, Some(f)) => {
                let path = dir.ok_or_else(|| format!("{where_}: \"file\" needs a plugin folder"))?.join(f);
                std::fs::read_to_string(&path).map_err(|e| format!("{where_}: {}: {e}", path.display()))?
            }
            (None, None) => return Err(format!("{where_}: no shader \"source\" or \"file\"")),
        };
        let sound = kind == EffectKind::Sound;
        if !sound && (self.shader.entry.is_empty() || !source.contains(&self.shader.entry)) {
            return Err(format!("{where_}: the shader has no function called \"{}\"", self.shader.entry));
        }
        let mut params = Vec::new();
        for p in self.params {
            params.push(p.into_schema().map_err(|e| format!("{where_}: {e}"))?);
        }
        if sound && usage == EffectUsage::Cut {
            return Err(format!("{where_}: a sound effect's usage is passive or in_out"));
        }
        let pointish = matches!(kind, EffectKind::PointOp | EffectKind::UvWarp);
        Ok(EffectDescriptor {
            type_id: self.id.into(),
            version: self.version,
            api_version: PLUGIN_API_VERSION,
            name: self.name,
            preserves_opacity: self.preserves_opacity.unwrap_or(kind == EffectKind::PointOp),
            time_varying: self.time_varying,
            usage,
            motion: None,
            pass_count: None,
            pass_divisor: None,
            fusible: pointish,
            kind,
            state: crate::registry::Statefulness::Pure,
            space,
            params,
            shader: Some(EffectShader { entry: self.shader.entry, source: source.into(), passes: self.shader.passes.max(1) }),
        })
    }
}

/// Reads one plugin folder (or a lone `plugin.json`). Bad effects are skipped and
/// reported in `issues`; only a broken manifest fails outright.
pub fn load(manifest_path: &Path) -> Result<Plugin, String> {
    let text = std::fs::read_to_string(manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    if manifest.api_version != PLUGIN_API_VERSION {
        return Err(format!(
            "{}: plugin API version {} — this host speaks {PLUGIN_API_VERSION}",
            manifest_path.display(),
            manifest.api_version
        ));
    }
    if manifest.id == CORE_ID {
        return Err(format!("{}: \"{CORE_ID}\" is the built-in plugin's id", manifest_path.display()));
    }
    let dir = manifest_path.parent();
    let (mut effects, mut issues) = (Vec::new(), Vec::new());
    for e in manifest.effects {
        match e.into_descriptor(dir) {
            Ok(d) => effects.push(d),
            Err(e) => issues.push(e),
        }
    }
    Ok(Plugin {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        description: manifest.description,
        author: manifest.author,
        builtin: false,
        path: Some(manifest_path.to_path_buf()),
        effects,
        issues,
    })
}

/// Every plugin under `dir`: each subfolder's `plugin.json`, plus any `*.plugin.json`
/// directly inside it. Returns them by name, and the ones that couldn't be read at all.
pub fn load_dir(dir: &Path) -> (Vec<Plugin>, Vec<String>) {
    let (mut plugins, mut errors) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(dir) else { return (plugins, errors) };
    let mut found: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let manifest = path.join(MANIFEST);
            if manifest.is_file() {
                found.push(manifest);
            }
        } else if path.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".plugin.json")) {
            found.push(path);
        }
    }
    found.sort();
    for path in found {
        match load(&path) {
            Ok(p) if plugins.iter().any(|e: &Plugin| e.id == p.id) => errors.push(format!("{}: another plugin already uses the id \"{}\"", path.display(), p.id)),
            Ok(p) => plugins.push(p),
            Err(e) => errors.push(e),
        }
    }
    plugins.sort_by_key(|p| p.name.to_lowercase());
    (plugins, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, text).expect("write");
        path
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oa-plugin-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn core_holds_the_builtins() {
        let core = core(vec![crate::registry::sound("oa.audio.echo", "Echo", EffectUsage::Passive, vec![], None)]);
        assert!(core.builtin && core.id == CORE_ID);
        assert!(core.effects.iter().any(|d| &*d.type_id == "oa.blur.gaussian"));
        assert!(core.offered_effects() < core.effects.len(), "internal effects aren't offered");
        assert!(core.summary().contains("1 sound effect"));
    }

    /// A folder with a manifest and a shader file becomes effects the registry can use.
    #[test]
    fn loads_a_plugin_folder() {
        let dir = temp("folder");
        write(&dir, "looks/vignette.wgsl", "fn vignette(c: vec4f, base: u32) -> vec4f { return c; }");
        write(
            &dir,
            "looks/plugin.json",
            r#"{
                "id": "com.example.looks", "name": "Example Looks", "version": "1.2.0",
                "description": "A look or two.", "author": "Someone",
                "effects": [{
                    "id": "com.example.vignette", "name": "Vignette", "kind": "point",
                    "params": [
                        {"id": "amount", "type": "float", "default": 0.5, "min": 0, "max": 1},
                        {"id": "edges", "type": "enum", "options": ["soft", "hard"], "static_only": true}
                    ],
                    "shader": {"entry": "vignette", "file": "vignette.wgsl"}
                }]
            }"#,
        );
        let (plugins, errors) = load_dir(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(plugins.len(), 1);
        let p = &plugins[0];
        assert_eq!((p.name.as_str(), p.version.as_str(), p.issues.len()), ("Example Looks", "1.2.0", 0));
        let d = &p.effects[0];
        assert_eq!(d.kind, EffectKind::PointOp);
        assert_eq!(d.params[0].range, Some((0.0, 1.0)));
        assert_eq!(d.params[1].default, Value::Enum("soft".into()));
        assert!(!d.params[1].animatable);
        assert!(d.shader.as_ref().is_some_and(|s| s.source.contains("fn vignette")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A sound effect is declared like any other, with a sound shader instead of WGSL.
    #[test]
    fn loads_sound_effects() {
        let dir = temp("sound");
        write(&dir, "noisy/tremolo.oasound", "out = in * (1 - depth * (0.5 + 0.5 * sin(TAU * 5 * time)));");
        let path = write(
            &dir,
            "noisy/plugin.json",
            r#"{
                "id": "com.example.noisy", "name": "Noisy",
                "effects": [
                    {"id": "com.example.tremolo", "name": "Wobble", "kind": "sound",
                     "params": [{"id": "depth", "type": "float", "default": 0.5, "min": 0, "max": 1}],
                     "shader": {"file": "tremolo.oasound"}},
                    {"id": "com.example.cut", "name": "Bad", "kind": "sound", "usage": "cut", "shader": {"source": "out = in;"}}
                ]
            }"#,
        );
        let p = load(&path).expect("loads");
        assert_eq!(p.effects.len(), 1, "{:?}", p.issues);
        assert_eq!(p.effects[0].kind, EffectKind::Sound);
        assert!(p.effects[0].shader.as_ref().is_some_and(|s| s.source.contains("depth")));
        assert_eq!((p.offered_effects(), p.sounds().count()), (0, 1));
        assert!(p.summary().contains("1 sound effect"));
        let (registry, issues) = crate::registry::Registry::from_plugins([&p]);
        assert!(issues.is_empty() && registry.is_sound("com.example.tremolo") && !registry.is_sound("com.example.vignette"));
        assert!(registry.offered(EffectUsage::Passive).is_empty(), "sound effects aren't offered as picture effects");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bad effects are reported, not fatal; reserved ids and wrong API versions are refused.
    #[test]
    fn reports_what_it_cannot_use() {
        let dir = temp("bad");
        let path = write(
            &dir,
            "x.plugin.json",
            r#"{
                "id": "com.example.x", "name": "X",
                "effects": [
                    {"id": "oa.color.exposure", "name": "Mine", "kind": "point", "shader": {"entry": "a", "source": "fn a() {}"}},
                    {"id": "com.example.b", "name": "B", "kind": "wobbly", "shader": {"entry": "b", "source": "fn b() {}"}},
                    {"id": "com.example.c", "name": "C", "kind": "point", "shader": {"entry": "c", "file": "nope.wgsl"}},
                    {"id": "com.example.d", "name": "D", "kind": "point", "shader": {"entry": "zz", "source": "fn d() {}"}},
                    {"id": "com.example.e", "name": "E", "kind": "point", "shader": {"entry": "e", "source": "fn e() {}"}}
                ]
            }"#,
        );
        let p = load(&path).expect("manifest parses");
        assert_eq!(p.effects.len(), 1, "only the good one: {:?}", p.issues);
        assert_eq!(p.issues.len(), 4);
        assert!(p.issues[0].contains("reserved"), "{:?}", p.issues);
        assert!(p.issues[3].contains("no function called"), "{:?}", p.issues);

        write(&dir, "old.plugin.json", r#"{"id": "com.example.old", "name": "Old", "api_version": 99}"#);
        let (plugins, errors) = load_dir(&dir);
        assert_eq!(plugins.len(), 1);
        assert!(errors[0].contains("plugin API version 99"), "{errors:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
