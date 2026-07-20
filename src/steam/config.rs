//! Steam-style controller mapping configuration.
//!
//! Files use Valve's KeyValues ("VDF") syntax — the same format Steam itself
//! stores controller configurations in — with a simplified subset of Steam's
//! `controller_mappings` schema (see `configs/*.vdf` for commented
//! examples):
//!
//! ```vdf
//! "controller_mappings"
//! {
//!     "version"   "1"
//!     "title"     "…"
//!     "group"
//!     {
//!         "source"    "button_diamond"    // which physical control
//!         "bindings"  { "button_a" "switch_button B" … }
//!     }
//!     "group"
//!     {
//!         "source"    "joystick"
//!         "mode"      "joystick_move"
//!         "bindings"  { "click" "switch_button LSTICK" }
//!         "settings"  { "output_joystick" "left" }
//!     }
//! }
//! ```
//!
//! Differences from Steam's real schema, chosen to stay close while fitting
//! what the bridge can do: binding values are `switch_button <NAME>` (Steam
//! emits `xinput_button <NAME>`); groups carry an explicit `source` (Steam
//! uses a separate `preset`/`group_source_bindings` table); only the modes
//! the bridge implements exist (`dpad` for the left pad, `joystick_move`
//! for pad/joystick, `joystick_camera` for the right pad, `trigger` for
//! full pulls). The left pad's `dpad` group honors Steam's `requires_click`
//! setting: `"1"` (default) presses on the click quadrants, `"0"` makes the
//! touch position alone drive the d-pad. The right pad's mode picks how it
//! drives its stick: `joystick_move` (default) maps the touch position
//! absolutely, `joystick_camera` turns finger *motion* into stick
//! deflection (mouse-like camera), tunable via
//! `settings { "sensitivity" "N" }`.
//!
//! A config is a *complete* layout: it starts from an empty mapping, and
//! anything it doesn't bind stays unbound. Unknown sources and binding keys
//! are warnings (so configs written for a future sweam still load); a
//! malformed binding value is an error.

use super::mapping::{self, LeftPadMode, Mapping, RightPadMode, StickTarget};
use crate::state::Button;
use crate::vdf;
use anyhow::{bail, Context};

pub fn load(path: &str) -> anyhow::Result<Mapping> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read config {path:?}"))?;
    parse(&text).with_context(|| format!("Failed to parse config {path:?}"))
}

pub fn parse(text: &str) -> anyhow::Result<Mapping> {
    let doc = vdf::parse(text)?;
    let root = doc
        .get_block("controller_mappings")
        .context("Missing top-level \"controller_mappings\" block")?;
    match root.get_str("version") {
        None | Some("1") => {}
        Some(other) => bail!("Unsupported config version {other:?} (expected \"1\")"),
    }

    let mut mapping = Mapping::empty();
    for group in root.get_all("group") {
        let vdf::Value::Block(group) = group else {
            bail!("\"group\" must be a block");
        };
        apply_group(&mut mapping, group)?;
    }
    Ok(mapping)
}

fn apply_group(mapping: &mut Mapping, group: &vdf::Block) -> anyhow::Result<()> {
    let source = group
        .get_str("source")
        .context("group is missing \"source\"")?;
    let bindings = group.get_block("bindings").cloned().unwrap_or_default();

    // Binding keys per source, following Steam's vocabulary.
    let button_keys: &[(&str, &[u16])] = match source {
        "button_diamond" => &[
            ("button_a", &[mapping::BTN_A]),
            ("button_b", &[mapping::BTN_B]),
            ("button_x", &[mapping::BTN_X]),
            ("button_y", &[mapping::BTN_Y]),
        ],
        "switch" => &[
            ("left_bumper", &[mapping::BTN_TL]),
            ("right_bumper", &[mapping::BTN_TR]),
            ("button_escape", &[mapping::BTN_SELECT]), // left menu (back)
            ("button_menu", &[mapping::BTN_START]),    // right menu (start)
            ("button_steam", &[mapping::BTN_MODE]),
            // Both grip encodings, see mapping.rs.
            (
                "button_back_left",
                &[mapping::BTN_GRIPL, mapping::BTN_GEAR_DOWN],
            ),
            (
                "button_back_right",
                &[mapping::BTN_GRIPR, mapping::BTN_GEAR_UP],
            ),
        ],
        "left_trigger" => &[("click", &[mapping::BTN_TL2])],
        "right_trigger" => &[("click", &[mapping::BTN_TR2])],
        "left_trackpad" => {
            mapping.left_pad = left_pad_mode(group).with_context(|| format!("group {source:?}"))?;
            &[
                ("dpad_north", &[mapping::BTN_DPAD_UP]),
                ("dpad_south", &[mapping::BTN_DPAD_DOWN]),
                ("dpad_west", &[mapping::BTN_DPAD_LEFT]),
                ("dpad_east", &[mapping::BTN_DPAD_RIGHT]),
            ]
        }
        "joystick" => {
            mapping.joystick = output_stick(group).with_context(|| format!("group {source:?}"))?;
            &[("click", &[mapping::BTN_THUMBL])]
        }
        "right_trackpad" => {
            mapping.right_pad = output_stick(group).with_context(|| format!("group {source:?}"))?;
            mapping.right_pad_mode =
                right_pad_mode(group).with_context(|| format!("group {source:?}"))?;
            mapping.camera_sensitivity =
                camera_sensitivity(group).with_context(|| format!("group {source:?}"))?;
            &[("click", &[mapping::BTN_THUMBR])]
        }
        other => {
            eprintln!("Config: ignoring unknown group source {other:?}");
            return Ok(());
        }
    };

    for (key, value) in &bindings.0 {
        let vdf::Value::String(value) = value else {
            bail!("binding {key:?} in group {source:?} must be a string");
        };
        let Some((_, codes)) = button_keys
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
        else {
            eprintln!("Config: ignoring unknown binding {key:?} in group {source:?}");
            continue;
        };
        let target =
            parse_binding(value).with_context(|| format!("binding {key:?} in group {source:?}"))?;
        if let Some(button) = target {
            for &code in *codes {
                mapping.bind_button(code, button);
            }
        }
    }
    Ok(())
}

/// `settings { "output_joystick" "left"|"right"|"none" }`; Steam's numeric
/// values (0 = left, 1 = right) are accepted too. Defaults per Steam's
/// convention: absent settings mean "the stick this control usually is".
fn output_stick(group: &vdf::Block) -> anyhow::Result<StickTarget> {
    let source = group.get_str("source").unwrap_or_default();
    let default = match source {
        "joystick" => StickTarget::LeftStick,
        _ => StickTarget::RightStick,
    };
    let Some(settings) = group.get_block("settings") else {
        return Ok(default);
    };
    match settings.get_str("output_joystick") {
        None => Ok(default),
        Some("left") | Some("0") => Ok(StickTarget::LeftStick),
        Some("right") | Some("1") => Ok(StickTarget::RightStick),
        Some("none") => Ok(StickTarget::None),
        Some(other) => bail!("bad output_joystick {other:?} (left|right|none)"),
    }
}

/// `settings { "requires_click" "1"|"0" }` on the left pad's dpad group —
/// Steam's own setting name for this: `"1"` (the default) presses directions
/// on the click quadrants, `"0"` lets the touch position alone drive them.
fn left_pad_mode(group: &vdf::Block) -> anyhow::Result<LeftPadMode> {
    let Some(settings) = group.get_block("settings") else {
        return Ok(LeftPadMode::ClickDpad);
    };
    match settings.get_str("requires_click") {
        None | Some("1") => Ok(LeftPadMode::ClickDpad),
        Some("0") => Ok(LeftPadMode::TouchDpad),
        Some(other) => bail!("bad requires_click {other:?} (0|1)"),
    }
}

/// `"mode"` on the right-pad group — Steam's own mode names: absent or
/// `joystick_move` (the default) maps the touch position absolutely,
/// `joystick_camera` turns finger motion into stick deflection. Unknown
/// modes warn and fall back to absolute, matching how unknown sources and
/// binding keys are treated.
fn right_pad_mode(group: &vdf::Block) -> anyhow::Result<RightPadMode> {
    match group.get_str("mode") {
        None | Some("joystick_move") => Ok(RightPadMode::AbsoluteStick),
        Some("joystick_camera") => Ok(RightPadMode::CameraStick),
        Some(other) => {
            eprintln!("Config: ignoring unknown right_trackpad mode {other:?}");
            Ok(RightPadMode::AbsoluteStick)
        }
    }
}

/// `settings { "sensitivity" "N" }` on the right-pad group: camera-mode
/// gain — stick deflection gained per unit of finger travel (both in evdev
/// units), default 4. Must be a positive number.
fn camera_sensitivity(group: &vdf::Block) -> anyhow::Result<f32> {
    let value = group
        .get_block("settings")
        .and_then(|settings| settings.get_str("sensitivity"));
    let Some(text) = value else {
        return Ok(mapping::CAMERA_SENSITIVITY_DEFAULT);
    };
    match text.parse::<f32>() {
        Ok(value) if value.is_finite() && value > 0.0 => Ok(value),
        _ => bail!("bad sensitivity {text:?} (expected a positive number)"),
    }
}

/// `switch_button <NAME>` or `none`. Steam values carry trailing activator
/// fields (`"xinput_button A, , "`) — tolerate and ignore them.
fn parse_binding(value: &str) -> anyhow::Result<Option<Button>> {
    let value = value.split(',').next().unwrap_or_default().trim();
    if value.eq_ignore_ascii_case("none") || value.is_empty() {
        return Ok(None);
    }
    let mut words = value.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(kind), Some(name), None) if kind.eq_ignore_ascii_case("switch_button") => {
            Ok(Some(button_by_name(name)?))
        }
        _ => bail!("bad binding value {value:?} (expected \"switch_button <NAME>\" or \"none\")"),
    }
}

fn button_by_name(name: &str) -> anyhow::Result<Button> {
    let found = Button::ALL
        .iter()
        .find(|button| button.name().eq_ignore_ascii_case(name));
    // describe()/name() use Up/Down/…, Steam-style configs say DPAD_UP/… —
    // accept both.
    let found = found.or_else(|| {
        let name = name.strip_prefix("DPAD_").or(name.strip_prefix("dpad_"))?;
        Button::ALL
            .iter()
            .find(|button| button.name().eq_ignore_ascii_case(name))
    });
    match found {
        Some(&button) => Ok(button),
        None => bail!("unknown switch_button {name:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::mapping::{
        BTN_A, BTN_DPAD_UP, BTN_GEAR_DOWN, BTN_GRIPL, BTN_THUMBL, BTN_TL2,
    };

    const DEFAULT: &str = include_str!("../../configs/default.vdf");
    const FACE_LABELS: &str = include_str!("../../configs/face-labels.vdf");
    const SWAPPED: &str = include_str!("../../configs/swapped-sticks.vdf");
    const TOUCH_DPAD: &str = include_str!("../../configs/touch-dpad.vdf");
    const CAMERA: &str = include_str!("../../configs/camera-rightpad.vdf");

    #[test]
    fn default_config_matches_builtin_mapping() {
        assert_eq!(parse(DEFAULT).unwrap(), Mapping::default());
    }

    #[test]
    fn face_labels_config_matches_by_label() {
        let mapping = parse(FACE_LABELS).unwrap();
        assert_eq!(mapping.button(BTN_A), Some(Button::A));
        // Everything else stays as in the default layout.
        assert_eq!(mapping.button(BTN_TL2), Some(Button::ZL));
        assert_eq!(mapping.joystick, StickTarget::LeftStick);
    }

    #[test]
    fn swapped_sticks_config_swaps_targets() {
        let mapping = parse(SWAPPED).unwrap();
        assert_eq!(mapping.joystick, StickTarget::RightStick);
        assert_eq!(mapping.right_pad, StickTarget::LeftStick);
        assert_eq!(mapping.button(BTN_THUMBL), Some(Button::RStick));
    }

    #[test]
    fn touch_dpad_config_selects_touch_mode() {
        let mapping = parse(TOUCH_DPAD).unwrap();
        assert_eq!(mapping.left_pad, LeftPadMode::TouchDpad);
        // The direction bindings still apply.
        assert_eq!(mapping.button(BTN_DPAD_UP), Some(Button::Up));
        // Everything else stays as in the default layout.
        assert_eq!(mapping.joystick, StickTarget::LeftStick);
    }

    #[test]
    fn requires_click_defaults_to_click_mode() {
        assert_eq!(parse(DEFAULT).unwrap().left_pad, LeftPadMode::ClickDpad);
        // An explicit "1" means the same thing.
        let mapping = parse(
            r#""controller_mappings" { "group" {
                "source" "left_trackpad"
                "settings" { "requires_click" "1" }
            } }"#,
        )
        .unwrap();
        assert_eq!(mapping.left_pad, LeftPadMode::ClickDpad);
    }

    #[test]
    fn camera_config_selects_camera_mode() {
        let mapping = parse(CAMERA).unwrap();
        assert_eq!(mapping.right_pad_mode, RightPadMode::CameraStick);
        // The example leaves sensitivity commented out → the default.
        assert_eq!(
            mapping.camera_sensitivity,
            mapping::CAMERA_SENSITIVITY_DEFAULT
        );
        // Everything else stays as in the default layout.
        assert_eq!(mapping.right_pad, StickTarget::RightStick);
        assert_eq!(mapping.joystick, StickTarget::LeftStick);
    }

    #[test]
    fn right_pad_mode_defaults_to_absolute() {
        // Both the default config (explicit "joystick_move") and a bare
        // group without a mode keep the absolute mapping.
        assert_eq!(
            parse(DEFAULT).unwrap().right_pad_mode,
            RightPadMode::AbsoluteStick
        );
        let mapping =
            parse(r#""controller_mappings" { "group" { "source" "right_trackpad" } }"#).unwrap();
        assert_eq!(mapping.right_pad_mode, RightPadMode::AbsoluteStick);
    }

    #[test]
    fn camera_sensitivity_is_configurable() {
        let mapping = parse(
            r#""controller_mappings" { "group" {
                "source" "right_trackpad"
                "mode" "joystick_camera"
                "settings" { "sensitivity" "2.5" }
            } }"#,
        )
        .unwrap();
        assert_eq!(mapping.camera_sensitivity, 2.5);
    }

    #[test]
    fn bad_camera_sensitivity_is_rejected() {
        for bad in ["fast", "-1", "0", "inf"] {
            let text = format!(
                r#""controller_mappings" {{ "group" {{
                    "source" "right_trackpad"
                    "settings" {{ "sensitivity" "{bad}" }}
                }} }}"#
            );
            assert!(parse(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn grip_bindings_cover_both_encodings() {
        let mapping = parse(DEFAULT).unwrap();
        assert_eq!(mapping.button(BTN_GRIPL), Some(Button::Capture));
        assert_eq!(mapping.button(BTN_GEAR_DOWN), Some(Button::Capture));
    }

    #[test]
    fn steam_style_trailing_activator_fields_are_tolerated() {
        let mapping = parse(
            r#""controller_mappings" { "group" {
                "source" "button_diamond"
                "bindings" { "button_a" "switch_button A, , " }
            } }"#,
        )
        .unwrap();
        assert_eq!(mapping.button(BTN_A), Some(Button::A));
    }

    #[test]
    fn none_and_unknown_keys_leave_inputs_unbound() {
        let mapping = parse(
            r#""controller_mappings" { "group" {
                "source" "button_diamond"
                "bindings" {
                    "button_a" "none"
                    "button_from_the_future" "switch_button A"
                }
            } }"#,
        )
        .unwrap();
        assert_eq!(mapping.button(BTN_A), None);
    }

    #[test]
    fn errors_are_rejected() {
        // Wrong version.
        assert!(parse(r#""controller_mappings" { "version" "3" }"#).is_err());
        // Not a controller mapping at all.
        assert!(parse(r#""something_else" { }"#).is_err());
        // Unknown button name.
        let bad_button = r#""controller_mappings" { "group" {
            "source" "button_diamond"
            "bindings" { "button_a" "switch_button WARP" }
        } }"#;
        assert!(parse(bad_button).is_err());
        // Bad output_joystick.
        let bad_output = r#""controller_mappings" { "group" {
            "source" "joystick"
            "settings" { "output_joystick" "up" }
        } }"#;
        assert!(parse(bad_output).is_err());
        // Bad requires_click.
        let bad_click = r#""controller_mappings" { "group" {
            "source" "left_trackpad"
            "settings" { "requires_click" "maybe" }
        } }"#;
        assert!(parse(bad_click).is_err());
    }

    #[test]
    fn dpad_names_accept_both_spellings() {
        for name in ["DPAD_UP", "UP", "dpad_up", "up"] {
            assert!(matches!(button_by_name(name), Ok(Button::Up)), "{name}");
        }
    }

    #[test]
    fn empty_mappings_block_gives_empty_mapping() {
        let mapping = parse(r#""controller_mappings" { "version" "1" }"#).unwrap();
        assert_eq!(mapping, Mapping::empty());
    }
}
