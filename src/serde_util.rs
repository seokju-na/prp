use anyhow::{Context, Result};
use semver::Version;
use serde_json::Value;
use std::collections::HashMap;

pub fn parse_required_str(data: &Value, field: &str) -> Result<String> {
  let str = data
    .get(field)
    .with_context(|| format!("Missing required field \"{}\"", field))?
    .as_str()
    .with_context(|| format!("Field \"{field}\" is not a string"))?
    .to_string();
  Ok(str)
}

pub fn parse_version(data: &Value, field: &str) -> Result<Option<Version>> {
  let Some(str) = data.get(field).and_then(|x| x.as_str()) else {
    return Ok(None);
  };
  let ver = Version::parse(str)
    .with_context(|| format!("Field \"{}\" is not a valid semver: {}", field, str))?;
  Ok(Some(ver))
}

pub fn parse_bool(data: &Value, field: &str) -> Result<Option<bool>> {
  data
    .get(field)
    .map(|value| {
      value
        .as_bool()
        .with_context(|| format!("Field \"{field}\" is not a boolean"))
    })
    .transpose()
}

pub fn parse_map(data: &Value, field: &str) -> Result<Option<HashMap<String, String>>> {
  data
    .get(field)
    .map(|value| {
      serde_json::from_value(value.clone())
        .with_context(|| format!("Field \"{field}\" must be an object of strings"))
    })
    .transpose()
}

pub fn string_list(value: &Value) -> Result<Vec<String>> {
  value
    .as_array()
    .context("Expected a list")?
    .iter()
    .map(|item| {
      item
        .as_str()
        .map(str::to_string)
        .context("Expected a string")
    })
    .collect()
}
