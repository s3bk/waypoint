//! Node types and edge types: a name plus how they look. A graph has a
//! registry of each; nodes and edges point at an entry by id. Validation lives
//! here so the server and tests agree on what a legal style is.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Smallest contrast ratio for text on its background (WCAG AA for normal text).
pub const MIN_CONTRAST: f64 = 4.5;
/// Smallest contrast ratio for a line against the background (WCAG non-text contrast).
pub const MIN_LINE_CONTRAST: f64 = 3.0;
/// Edge widths are clamped to this range, in pixels at zoom 1; a node border may also be 0 (none).
pub const MIN_EDGE_WIDTH: u32 = 1;
pub const MAX_WIDTH: u32 = 10;
const MAX_NAME: usize = 64;
const MAX_LABEL: usize = 40;

pub const NODE_SHAPES: &[&str] = &["rect", "pill", "ellipse", "hexagon", "rhombus"];
/// How the corners of a `rect`, `hexagon` or `rhombus` look; `pill` and `ellipse` have none.
pub const CORNERS: &[&str] = &["round", "square"];
pub const ARROWS: &[&str] = &["none", "arrow", "dot", "bar"];
pub const LINE_STYLES: &[&str] = &["solid", "dashed", "dotted"];

/// The canvas colour of a graph nobody has recoloured.
pub const DEFAULT_BACKGROUND: &str = "#14161a";

/// The seeded node type, the same id in every graph. Untyped nodes use it.
pub const DEFAULT_NODE_TYPE: Uuid = Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_0101);
/// The seeded edge type (`EdgeTypeId::DEPENDS_ON`). Untyped edges use it.
pub const DEFAULT_EDGE_TYPE: Uuid = Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_0001);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Node,
    Edge,
}

/// A node type. `bg` fills the box, `fg` colours the title and the two are
/// set together; `border` outlines it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeType {
    pub id: Uuid,
    /// Handle that people and agents use: lowercase `[a-z0-9_.-]+`. Unique per registry.
    pub name: String,
    pub shape: String,
    pub bg: String,
    pub fg: String,
    pub border: String,
    pub border_width: u32,
    pub corners: String,
}

/// An edge type. The line and its text are drawn straight on the document
/// background, so `color` (3:1) and `text_color` (4.5:1) must contrast with that.
/// `width` is clamped to 1-10.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeType {
    pub id: Uuid,
    pub name: String,
    /// The text drawn along the edge.
    pub label: String,
    pub color: String,
    pub width: u32,
    pub line_style: String,
    /// What the line ends in at the dependant (`from`) end.
    pub start: String,
    /// What the line ends in at the dependency (`to`) end.
    pub end: String,
    pub text_color: String,
}

/// Fields of a node type to set; anything left out stays (or takes the default, for a new type).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeSpec {
    pub name: String,
    /// Rename the type (it keeps its id, so nodes follow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    /// Background and foreground are set together: give both or neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corners: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgeSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
}

pub fn builtin_node() -> NodeType {
    NodeType {
        id: DEFAULT_NODE_TYPE,
        name: "default".into(),
        shape: "rect".into(),
        bg: "#2b3340".into(),
        fg: "#e6e9ef".into(),
        border: "#4a5568".into(),
        border_width: 1,
        corners: "round".into(),
    }
}

pub fn builtin_edge() -> EdgeType {
    EdgeType {
        id: DEFAULT_EDGE_TYPE,
        name: "depends_on".into(),
        label: "depends on".into(),
        color: "#8aa0bd".into(),
        width: 2,
        line_style: "solid".into(),
        start: "none".into(),
        end: "arrow".into(),
        text_color: "#8793a8".into(),
    }
}

/// `#rrggbb` (any case, or `#rgb`) as lowercase `#rrggbb`.
pub fn normalize_color(s: &str) -> Result<String, String> {
    let hex = s.trim().strip_prefix('#').unwrap_or("");
    let ok = hex.bytes().all(|b| b.is_ascii_hexdigit());
    match (ok, hex.len()) {
        (true, 6) => Ok(format!("#{}", hex.to_ascii_lowercase())),
        (true, 3) => Ok(format!("#{}", hex.chars().flat_map(|c| [c, c]).collect::<String>().to_ascii_lowercase())),
        _ => Err(format!("{s:?} is not a colour: use #rrggbb")),
    }
}

fn luminance(color: &str) -> f64 {
    let lin = |c: f64| if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let v = |i: usize| lin(u8::from_str_radix(&color[1 + 2 * i..3 + 2 * i], 16).unwrap_or(0) as f64 / 255.0);
    0.2126 * v(0) + 0.7152 * v(1) + 0.0722 * v(2)
}

/// WCAG contrast ratio of two `#rrggbb` colours, 1.0 to 21.0.
pub fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Black or white, whichever reads better on `bg`.
pub fn readable_on(bg: &str) -> &'static str {
    if contrast(bg, "#ffffff") >= contrast(bg, "#000000") { "#ffffff" } else { "#000000" }
}

pub fn normalize_name(name: &str) -> Result<String, String> {
    let n = name.trim().to_ascii_lowercase();
    let ok = !n.is_empty()
        && n.len() <= MAX_NAME
        && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.'));
    if ok { Ok(n) } else { Err(format!("type name {name:?} must be 1-{MAX_NAME} characters of a-z, 0-9, '_', '-', '.'")) }
}

/// Check that text of colour `fg` can be read on `bg`; `what` names the pair in the message.
pub fn check_contrast(what: &str, bg: &str, fg: &str) -> Result<(), String> {
    let ratio = contrast(bg, fg);
    if ratio < MIN_CONTRAST {
        Err(format!("{what} {fg} on {bg} has contrast {ratio:.1}:1; at least {MIN_CONTRAST}:1 is needed"))
    } else {
        Ok(())
    }
}

/// Check that a line of colour `color` can be seen on `bg`.
pub fn check_line_contrast(what: &str, bg: &str, color: &str) -> Result<(), String> {
    let ratio = contrast(bg, color);
    if ratio < MIN_LINE_CONTRAST {
        Err(format!("{what} {color} on {bg} has contrast {ratio:.1}:1; at least {MIN_LINE_CONTRAST}:1 is needed"))
    } else {
        Ok(())
    }
}

/// An edge label may be empty: then nothing is drawn along the edge.
fn label(l: &str) -> Result<String, String> {
    let l = l.split_whitespace().collect::<Vec<_>>().join(" ");
    if l.chars().count() > MAX_LABEL { Err(format!("label must be at most {MAX_LABEL} characters")) } else { Ok(l) }
}

pub fn edge_width(w: u32) -> u32 {
    w.clamp(MIN_EDGE_WIDTH, MAX_WIDTH)
}

pub fn border_width(w: u32) -> u32 {
    w.min(MAX_WIDTH)
}

fn one_of(value: &str, what: &str, allowed: &[&str]) -> Result<String, String> {
    if allowed.contains(&value) { Ok(value.to_owned()) } else { Err(format!("{value:?} is not a {what}; use one of {}", allowed.join(", "))) }
}

/// Apply `spec` to `base` (the existing entry, or the defaults for a new one).
pub fn apply_node(mut base: NodeType, spec: &NodeSpec) -> Result<NodeType, String> {
    if let Some(n) = &spec.new_name {
        base.name = normalize_name(n)?;
    }
    if let Some(s) = &spec.shape {
        // "rounded" was a shape of its own before corners existed.
        let (shape, round) = if s == "rounded" { ("rect", true) } else { (s.as_str(), false) };
        base.shape = one_of(shape, "node shape", NODE_SHAPES)?;
        if round && spec.corners.is_none() {
            base.corners = "round".into();
        }
    }
    match (&spec.bg, &spec.fg) {
        (Some(bg), Some(fg)) => {
            base.bg = normalize_color(bg)?;
            base.fg = normalize_color(fg)?;
        }
        (None, None) => {}
        _ => return Err("background and foreground colours are set together: give both `bg` and `fg`".into()),
    }
    check_contrast("foreground", &base.bg, &base.fg)?;
    if let Some(c) = &spec.border {
        base.border = normalize_color(c)?;
    }
    if let Some(w) = spec.border_width {
        base.border_width = border_width(w);
    }
    if let Some(s) = &spec.corners {
        base.corners = one_of(s, "corner style", CORNERS)?;
    }
    Ok(base)
}

/// Apply `spec` to `base`. `background` is the document's colour, which the text colour is checked against.
pub fn apply_edge(mut base: EdgeType, spec: &EdgeSpec, background: &str) -> Result<EdgeType, String> {
    if let Some(n) = &spec.new_name {
        base.name = normalize_name(n)?;
    }
    if let Some(l) = &spec.label {
        base.label = label(l)?;
    }
    if let Some(c) = &spec.color {
        base.color = normalize_color(c)?;
    }
    if let Some(w) = spec.width {
        base.width = edge_width(w);
    }
    if let Some(s) = &spec.line_style {
        base.line_style = one_of(s, "line style", LINE_STYLES)?;
    }
    if let Some(s) = &spec.start {
        base.start = one_of(s, "arrow type", ARROWS)?;
    }
    if let Some(s) = &spec.end {
        base.end = one_of(s, "arrow type", ARROWS)?;
    }
    if let Some(c) = &spec.text_color {
        base.text_color = normalize_color(c)?;
    }
    check_contrast("text_color", background, &base.text_color)
        .map_err(|e| format!("{e} (the document background is {background})"))?;
    check_line_contrast("color", background, &base.color)
        .map_err(|e| format!("{e} (the document background is {background})"))?;
    Ok(base)
}

pub fn new_node(spec: &NodeSpec) -> Result<NodeType, String> {
    let name = normalize_name(&spec.name)?;
    let base = NodeType { id: Uuid::now_v7(), name, ..builtin_node() };
    apply_node(base, &NodeSpec { new_name: None, ..spec.clone() })
}

/// A new edge type. Colours the spec leaves out are black or white, whichever reads on `background`.
pub fn new_edge(spec: &EdgeSpec, background: &str) -> Result<EdgeType, String> {
    let name = normalize_name(&spec.name)?;
    let base = EdgeType { id: Uuid::now_v7(), label: name.clone(), name, color: readable_on(background).into(), text_color: readable_on(background).into(), ..builtin_edge() };
    apply_edge(base, &EdgeSpec { new_name: None, ..spec.clone() }, background)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_matches_known_values() {
        assert!((contrast("#000000", "#ffffff") - 21.0).abs() < 0.01);
        assert!((contrast("#777777", "#ffffff") - 4.48).abs() < 0.02);
        assert!((contrast("#ffffff", "#ffffff") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn builtins_are_readable() {
        let n = builtin_node();
        assert!(contrast(&n.bg, &n.fg) >= MIN_CONTRAST);
        assert!(contrast(DEFAULT_BACKGROUND, &builtin_edge().text_color) >= MIN_CONTRAST);
        assert!(contrast(DEFAULT_BACKGROUND, &builtin_edge().color) >= MIN_LINE_CONTRAST);
    }

    #[test]
    fn colours_are_normalized_and_checked() {
        assert_eq!(normalize_color("#ABC").unwrap(), "#aabbcc");
        assert!(normalize_color("red").is_err());
        assert!(normalize_color("#12345").is_err());
    }

    #[test]
    fn node_background_and_foreground_come_together_and_must_contrast() {
        let spec = |bg: Option<&str>, fg: Option<&str>| NodeSpec {
            name: "t".into(),
            bg: bg.map(Into::into),
            fg: fg.map(Into::into),
            ..NodeSpec::default()
        };
        assert!(new_node(&spec(Some("#000000"), None)).unwrap_err().contains("together"));
        assert!(new_node(&spec(Some("#777777"), Some("#888888"))).unwrap_err().contains("contrast"));
        let ok = new_node(&spec(Some("#FFF"), Some("#000"))).unwrap();
        assert_eq!((ok.bg.as_str(), ok.fg.as_str()), ("#ffffff", "#000000"));
    }

    #[test]
    fn edge_text_must_read_on_the_document_background() {
        let spec = EdgeSpec { name: "e".into(), text_color: Some("#222222".into()), ..EdgeSpec::default() };
        assert!(new_edge(&spec, DEFAULT_BACKGROUND).unwrap_err().contains("contrast"));
        assert!(new_edge(&spec, "#ffffff").is_ok());
        // With no colour given, a new type picks one that reads.
        let plain = new_edge(&EdgeSpec { name: "e".into(), ..EdgeSpec::default() }, "#ffffff").unwrap();
        assert_eq!(plain.text_color, "#000000");
    }

    #[test]
    fn edge_line_must_be_visible_and_width_is_clamped() {
        let dim = EdgeSpec { name: "e".into(), color: Some("#1a1c20".into()), ..EdgeSpec::default() };
        assert!(new_edge(&dim, DEFAULT_BACKGROUND).unwrap_err().contains("color"));
        let wide = EdgeSpec { name: "e".into(), width: Some(99), ..EdgeSpec::default() };
        assert_eq!(new_edge(&wide, DEFAULT_BACKGROUND).unwrap().width, 10);
        let thin = EdgeSpec { name: "e".into(), width: Some(0), ..EdgeSpec::default() };
        assert_eq!(new_edge(&thin, DEFAULT_BACKGROUND).unwrap().width, 1);
    }

    #[test]
    fn shapes_and_arrows_are_checked() {
        let bad = NodeSpec { name: "t".into(), shape: Some("arrow".into()), ..NodeSpec::default() };
        assert!(new_node(&bad).is_err());
        let bad = EdgeSpec { name: "t".into(), end: Some("rounded".into()), ..EdgeSpec::default() };
        assert!(new_edge(&bad, DEFAULT_BACKGROUND).is_err());
        let ok = EdgeSpec { name: "t".into(), start: Some("dot".into()), end: Some("bar".into()), width: Some(4), ..EdgeSpec::default() };
        assert!(new_edge(&ok, DEFAULT_BACKGROUND).is_ok());
    }

    #[test]
    fn corners_and_shapes_for_nodes() {
        let spec = |shape: &str, corners: Option<&str>| NodeSpec {
            name: "t".into(),
            shape: Some(shape.into()),
            corners: corners.map(Into::into),
            ..NodeSpec::default()
        };
        assert_eq!(new_node(&spec("rhombus", Some("square"))).unwrap().corners, "square");
        assert!(new_node(&spec("rect", Some("bevel"))).is_err());
        // The old "rounded" shape is a rect with round corners.
        let old = new_node(&spec("rounded", None)).unwrap();
        assert_eq!((old.shape.as_str(), old.corners.as_str()), ("rect", "round"));
        assert_eq!(builtin_node().corners, "round");
    }

    #[test]
    fn an_edge_label_may_be_empty_but_not_long() {
        let spec = |l: &str| EdgeSpec { name: "e".into(), label: Some(l.into()), ..EdgeSpec::default() };
        assert_eq!(new_edge(&spec(""), DEFAULT_BACKGROUND).unwrap().label, "");
        assert_eq!(new_edge(&spec("  a   b "), DEFAULT_BACKGROUND).unwrap().label, "a b");
        assert!(new_edge(&spec(&"x".repeat(41)), DEFAULT_BACKGROUND).is_err());
        // Left out, it is the name.
        assert_eq!(new_edge(&EdgeSpec { name: "e".into(), ..EdgeSpec::default() }, DEFAULT_BACKGROUND).unwrap().label, "e");
    }
}
