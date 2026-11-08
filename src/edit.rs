use anyhow::{Context, Result, ensure};
use std::fmt;
use std::ops::Range;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch(String);

impl Patch {
  pub fn new(original: &str, modified: &str) -> Result<Self> {
    diffy::create_patch(original, modified).to_string().parse()
  }

  pub fn as_diffy(&self) -> diffy::Patch<'_, str> {
    diffy::Patch::from_str(&self.0).unwrap()
  }
}

impl Patch {
  /// The unified diff, with ANSI colors when `colors`.
  pub fn render(&self, colors: bool) -> String {
    match colors {
      true => diffy::PatchFormatter::new()
        .with_color()
        .fmt_patch(&self.as_diffy())
        .to_string(),
      false => self.0.clone(),
    }
  }
}

impl FromStr for Patch {
  type Err = anyhow::Error;

  fn from_str(text: &str) -> Result<Self> {
    diffy::Patch::from_str(text).context("Invalid patch")?;
    Ok(Self(text.to_string()))
  }
}

impl fmt::Display for Patch {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// A key path to an existing string value and its replacement,
/// such as `(vec!["package".into(), "version".into()], "2.0.0".into())`.
pub type Edit = (Vec<String>, String);

/// Replace existing string values in a JSON or JSONC document and return the
/// patch.
pub fn edit_json(raw: &str, edits: &[Edit]) -> Result<Patch> {
  let ast = jsonc_parser::parse_to_ast(raw, &Default::default(), &Default::default())
    .context("Failed to parse JSON")?;
  let root = ast.value.context("JSON document is empty")?;
  let mut replacements = Vec::with_capacity(edits.len());
  for (path, next) in edits {
    let (last, parents) = path.split_last().context("Edit path must not be empty")?;
    let mut object = root.as_object().context("JSON root must be an object")?;
    for (index, key) in parents.iter().enumerate() {
      object = object
        .get(key)
        .and_then(|prop| prop.value.as_object())
        .with_context(|| format!("Field \"{}\" must be an object", path[..=index].join(".")))?;
    }
    let value = object
      .get(last)
      .with_context(|| format!("Missing field \"{}\"", path.join(".")))?
      .value
      .as_string_lit()
      .with_context(|| format!("Field \"{}\" is not a string", path.join(".")))?;
    replacements.push((
      value.range.start..value.range.end,
      serde_json::to_string(next)?,
    ));
  }
  create_patch(raw, replacements)
}

/// Replace existing string values in a TOML document and return the patch.
pub fn edit_toml(raw: &str, edits: &[Edit]) -> Result<Patch> {
  let document = toml_edit::Document::parse(raw).context("Failed to parse TOML")?;
  let mut replacements = Vec::with_capacity(edits.len());
  for (path, next) in edits {
    ensure!(!path.is_empty(), "Edit path must not be empty");
    let mut item = document.as_item();
    for key in path {
      item = item
        .get(key.as_str())
        .with_context(|| format!("Missing field \"{}\"", path.join(".")))?;
    }
    let value = item
      .as_value()
      .filter(|value| value.is_str())
      .with_context(|| format!("Field \"{}\" is not a string", path.join(".")))?;
    let span = value
      .span()
      .with_context(|| format!("Field \"{}\" has no source position", path.join(".")))?;
    let literal = toml_string_literal(&raw[span.clone()], next);
    replacements.push((span, literal));
  }
  create_patch(raw, replacements)
}

/// Apply a patch produced by `edit_json` or `edit_toml`.
pub fn apply_patch(raw: &str, patch: &Patch) -> Result<String> {
  diffy::apply(raw, &patch.as_diffy()).context("Failed to apply patch")
}

fn create_patch(raw: &str, mut replacements: Vec<(Range<usize>, String)>) -> Result<Patch> {
  replacements.sort_by_key(|(range, _)| range.start);
  ensure!(
    replacements
      .windows(2)
      .all(|pair| pair[0].0.end <= pair[1].0.start),
    "Edits must not target the same value twice"
  );
  let mut modified = raw.to_string();
  for (range, text) in replacements.into_iter().rev() {
    modified.replace_range(range, &text);
  }
  Patch::new(raw, &modified)
}

// Literal strings ('...') have no escapes, so keep them only when the value fits.
fn toml_string_literal(old: &str, next: &str) -> String {
  if old.starts_with('\'') && !old.starts_with("'''") && !next.contains(['\'', '\n', '\r']) {
    format!("'{next}'")
  } else {
    toml_edit::Value::from(next).to_string()
  }
}
