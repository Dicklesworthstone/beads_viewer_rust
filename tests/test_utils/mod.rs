/// Strict JSON comparator and schema-validation test utilities.
///
/// Provides ordering-invariant array comparisons for unordered collections
/// and strict field-level checks for contract validation. Produces clear
/// diff output showing exact field paths, expected vs actual values.
use serde_json::Value;

// ============================================================================
// Schema Validation: verify structural contracts of robot JSON output
// ============================================================================

/// Validate that a JSON value has the expected fields at the given path.
/// Returns a list of violations (empty = valid).
#[must_use]
pub fn validate_fields(value: &Value, required_fields: &[&str], path: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(obj) = value.as_object() else {
        errors.push(format!("{path}: expected object, got {}", type_name(value)));
        return errors;
    };
    for &field in required_fields {
        if !obj.contains_key(field) {
            errors.push(format!("{path}: missing required field '{field}'"));
        }
    }
    errors
}

/// Validate that the robot output envelope has the standard fields.
#[must_use]
pub fn validate_envelope(value: &Value) -> Vec<String> {
    let mut errors = validate_fields(value, &["generated_at", "data_hash"], "");
    if let Some(ts) = value.get("generated_at").and_then(Value::as_str) {
        if chrono::DateTime::parse_from_rfc3339(ts).is_err() {
            errors.push(format!("generated_at: invalid RFC3339 timestamp '{ts}'"));
        }
    }
    if let Some(hash) = value.get("data_hash").and_then(Value::as_str) {
        if hash.is_empty() {
            errors.push("data_hash: empty string".to_string());
        }
    }
    errors
}

/// Validate output_format and version fields (newer robot outputs).
#[must_use]
pub fn validate_version_envelope(value: &Value) -> Vec<String> {
    let mut errors = validate_envelope(value);
    errors.extend(validate_fields(value, &["output_format", "version"], ""));
    if let Some(fmt) = value.get("output_format").and_then(Value::as_str) {
        if fmt != "json" {
            errors.push(format!("output_format: expected 'json', got '{fmt}'"));
        }
    }
    if let Some(ver) = value.get("version").and_then(Value::as_str) {
        if !ver.starts_with('v') {
            errors.push(format!("version: expected 'v' prefix, got '{ver}'"));
        }
    }
    errors
}

/// Validate that a nested path exists and is the expected type.
#[must_use]
pub fn validate_type_at(value: &Value, path: &str, expected_type: JsonType) -> Vec<String> {
    let mut errors = Vec::new();
    let resolved = resolve_path(value, path);
    match resolved {
        None => {
            errors.push(format!("{path}: path not found"));
        }
        Some(v) => {
            let actual = json_type(v);
            if actual != expected_type {
                errors.push(format!(
                    "{path}: expected type {expected_type:?}, got {actual:?}"
                ));
            }
        }
    }
    errors
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonType {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

fn json_type(v: &Value) -> JsonType {
    match v {
        Value::Null => JsonType::Null,
        Value::Bool(_) => JsonType::Bool,
        Value::Number(_) => JsonType::Number,
        Value::String(_) => JsonType::String,
        Value::Array(_) => JsonType::Array,
        Value::Object(_) => JsonType::Object,
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn resolve_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    let mut current = value;
    for segment in path.split('.') {
        // Try array index first
        if let Ok(index) = segment.parse::<usize>() {
            current = current.get(index)?;
        } else {
            current = current.get(segment)?;
        }
    }
    Some(current)
}

// ============================================================================
// Ordering-Invariant Comparisons
// ============================================================================

/// Compare two JSON values, treating arrays as unordered sets when `sort_key`
/// is provided. Returns a list of differences.
///
/// When `sort_key` is `None`, arrays are compared in order (strict).
/// When `sort_key` is `Some("field")`, array elements are sorted by that
/// field before comparison (ordering-invariant).
#[must_use]
pub fn compare_json(
    expected: &Value,
    actual: &Value,
    path: &str,
    sort_key: Option<&str>,
) -> Vec<Diff> {
    let mut diffs = Vec::new();
    compare_recursive(expected, actual, path, sort_key, &mut diffs);
    diffs
}

/// Compare two JSON values structurally, ignoring values of specified fields
/// (e.g., timestamps that vary between runs).
#[must_use]
pub fn compare_json_ignoring(
    expected: &Value,
    actual: &Value,
    path: &str,
    ignore_fields: &[&str],
) -> Vec<Diff> {
    let mut diffs = Vec::new();
    compare_recursive_ignoring(expected, actual, path, ignore_fields, &mut diffs);
    diffs
}

#[derive(Debug, Clone)]
pub struct Diff {
    pub path: String,
    pub kind: DiffKind,
}

#[derive(Debug, Clone)]
pub enum DiffKind {
    TypeMismatch { expected: String, actual: String },
    ValueMismatch { expected: String, actual: String },
    MissingField { field: String },
    ExtraField { field: String },
    ArrayLengthMismatch { expected: usize, actual: usize },
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            DiffKind::TypeMismatch { expected, actual } => {
                write!(
                    f,
                    "{}: type mismatch (expected {expected}, got {actual})",
                    self.path
                )
            }
            DiffKind::ValueMismatch { expected, actual } => {
                write!(
                    f,
                    "{}: value mismatch (expected {expected}, got {actual})",
                    self.path
                )
            }
            DiffKind::MissingField { field } => {
                write!(f, "{}: missing field '{field}'", self.path)
            }
            DiffKind::ExtraField { field } => {
                write!(f, "{}: extra field '{field}'", self.path)
            }
            DiffKind::ArrayLengthMismatch { expected, actual } => {
                write!(
                    f,
                    "{}: array length mismatch (expected {expected}, got {actual})",
                    self.path
                )
            }
        }
    }
}

fn compare_recursive(
    expected: &Value,
    actual: &Value,
    path: &str,
    sort_key: Option<&str>,
    diffs: &mut Vec<Diff>,
) {
    match (expected, actual) {
        (Value::Object(exp_obj), Value::Object(act_obj)) => {
            for (key, exp_val) in exp_obj {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match act_obj.get(key) {
                    Some(act_val) => {
                        compare_recursive(exp_val, act_val, &child_path, sort_key, diffs);
                    }
                    None => {
                        diffs.push(Diff {
                            path: path.to_string(),
                            kind: DiffKind::MissingField { field: key.clone() },
                        });
                    }
                }
            }
            for key in act_obj.keys() {
                if !exp_obj.contains_key(key) {
                    diffs.push(Diff {
                        path: path.to_string(),
                        kind: DiffKind::ExtraField { field: key.clone() },
                    });
                }
            }
        }
        (Value::Array(exp_arr), Value::Array(act_arr)) => {
            if exp_arr.len() != act_arr.len() {
                diffs.push(Diff {
                    path: path.to_string(),
                    kind: DiffKind::ArrayLengthMismatch {
                        expected: exp_arr.len(),
                        actual: act_arr.len(),
                    },
                });
                return;
            }
            // If sort_key provided and elements are objects, sort both by that key
            if let Some(key) = sort_key {
                let mut exp_sorted = exp_arr.clone();
                let mut act_sorted = act_arr.clone();
                exp_sorted.sort_by(|a, b| sort_by_key(a, b, key));
                act_sorted.sort_by(|a, b| sort_by_key(a, b, key));
                for (i, (exp_elem, act_elem)) in
                    exp_sorted.iter().zip(act_sorted.iter()).enumerate()
                {
                    let elem_path = format!("{path}[{i}]");
                    compare_recursive(exp_elem, act_elem, &elem_path, sort_key, diffs);
                }
            } else {
                for (i, (exp_elem, act_elem)) in exp_arr.iter().zip(act_arr.iter()).enumerate() {
                    let elem_path = format!("{path}[{i}]");
                    compare_recursive(exp_elem, act_elem, &elem_path, sort_key, diffs);
                }
            }
        }
        _ => {
            if json_type(expected) != json_type(actual) {
                diffs.push(Diff {
                    path: path.to_string(),
                    kind: DiffKind::TypeMismatch {
                        expected: type_name(expected).to_string(),
                        actual: type_name(actual).to_string(),
                    },
                });
            } else if expected != actual {
                diffs.push(Diff {
                    path: path.to_string(),
                    kind: DiffKind::ValueMismatch {
                        expected: format_value(expected),
                        actual: format_value(actual),
                    },
                });
            }
        }
    }
}

fn compare_recursive_ignoring(
    expected: &Value,
    actual: &Value,
    path: &str,
    ignore_fields: &[&str],
    diffs: &mut Vec<Diff>,
) {
    match (expected, actual) {
        (Value::Object(exp_obj), Value::Object(act_obj)) => {
            for (key, exp_val) in exp_obj {
                if ignore_fields.contains(&key.as_str()) {
                    continue;
                }
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match act_obj.get(key) {
                    Some(act_val) => {
                        compare_recursive_ignoring(
                            exp_val,
                            act_val,
                            &child_path,
                            ignore_fields,
                            diffs,
                        );
                    }
                    None => {
                        diffs.push(Diff {
                            path: path.to_string(),
                            kind: DiffKind::MissingField { field: key.clone() },
                        });
                    }
                }
            }
            for key in act_obj.keys() {
                if !ignore_fields.contains(&key.as_str()) && !exp_obj.contains_key(key) {
                    diffs.push(Diff {
                        path: path.to_string(),
                        kind: DiffKind::ExtraField { field: key.clone() },
                    });
                }
            }
        }
        (Value::Array(exp_arr), Value::Array(act_arr)) => {
            if exp_arr.len() != act_arr.len() {
                diffs.push(Diff {
                    path: path.to_string(),
                    kind: DiffKind::ArrayLengthMismatch {
                        expected: exp_arr.len(),
                        actual: act_arr.len(),
                    },
                });
                return;
            }
            for (i, (exp_elem, act_elem)) in exp_arr.iter().zip(act_arr.iter()).enumerate() {
                let elem_path = format!("{path}[{i}]");
                compare_recursive_ignoring(exp_elem, act_elem, &elem_path, ignore_fields, diffs);
            }
        }
        _ => {
            if json_type(expected) != json_type(actual) {
                diffs.push(Diff {
                    path: path.to_string(),
                    kind: DiffKind::TypeMismatch {
                        expected: type_name(expected).to_string(),
                        actual: type_name(actual).to_string(),
                    },
                });
            } else if expected != actual {
                diffs.push(Diff {
                    path: path.to_string(),
                    kind: DiffKind::ValueMismatch {
                        expected: format_value(expected),
                        actual: format_value(actual),
                    },
                });
            }
        }
    }
}

fn sort_by_key(a: &Value, b: &Value, key: &str) -> std::cmp::Ordering {
    let a_val = a.get(key).and_then(Value::as_str).unwrap_or("");
    let b_val = b.get(key).and_then(Value::as_str).unwrap_or("");
    a_val.cmp(b_val)
}

fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

// ============================================================================
// Assertion Helpers
// ============================================================================

/// Assert that a robot output passes envelope validation.
/// Panics with detailed errors if validation fails.
pub fn assert_valid_envelope(value: &Value) {
    let errors = validate_envelope(value);
    assert!(
        errors.is_empty(),
        "Envelope validation failed:\n{}",
        errors.join("\n")
    );
}

/// Assert that a robot output passes version envelope validation.
pub fn assert_valid_version_envelope(value: &Value) {
    let errors = validate_version_envelope(value);
    assert!(
        errors.is_empty(),
        "Version envelope validation failed:\n{}",
        errors.join("\n")
    );
}

/// Assert that two JSON values are structurally equal, with optional
/// ordering-invariant array comparison by sort key.
pub fn assert_json_eq(expected: &Value, actual: &Value, sort_key: Option<&str>) {
    let diffs = compare_json(expected, actual, "", sort_key);
    assert!(
        diffs.is_empty(),
        "JSON comparison failed ({} difference(s)):\n{}",
        diffs.len(),
        diffs
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Assert that two JSON values are equal, ignoring specified fields.
pub fn assert_json_eq_ignoring(expected: &Value, actual: &Value, ignore_fields: &[&str]) {
    let diffs = compare_json_ignoring(expected, actual, "", ignore_fields);
    assert!(
        diffs.is_empty(),
        "JSON comparison failed ({} difference(s), ignoring {:?}):\n{}",
        diffs.len(),
        ignore_fields,
        diffs
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Assert required fields exist at a path.
pub fn assert_has_fields(value: &Value, fields: &[&str], path: &str) {
    let errors = validate_fields(value, fields, path);
    assert!(
        errors.is_empty(),
        "Field validation failed:\n{}",
        errors.join("\n")
    );
}

/// Assert a value at a JSON path has the expected type.
pub fn assert_type_at(value: &Value, path: &str, expected_type: JsonType) {
    let errors = validate_type_at(value, path, expected_type);
    assert!(
        errors.is_empty(),
        "Type validation failed:\n{}",
        errors.join("\n")
    );
}

/// Format diffs as a compact report (for CI).
#[must_use]
pub fn format_diffs_compact(diffs: &[Diff]) -> String {
    diffs
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format diffs as a verbose report (for local debugging).
#[must_use]
pub fn format_diffs_verbose(diffs: &[Diff]) -> String {
    let mut output = String::new();
    for (i, diff) in diffs.iter().enumerate() {
        output.push_str(&format!("[{}] {diff}\n", i + 1));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_envelope_accepts_valid() {
        let v = json!({
            "generated_at": "2026-03-04T07:00:00Z",
            "data_hash": "abc123"
        });
        assert!(validate_envelope(&v).is_empty());
    }

    #[test]
    fn validate_envelope_rejects_missing_hash() {
        let v = json!({ "generated_at": "2026-03-04T07:00:00Z" });
        let errors = validate_envelope(&v);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("data_hash"));
    }

    #[test]
    fn validate_version_envelope_checks_format() {
        let v = json!({
            "generated_at": "2026-03-04T07:00:00Z",
            "data_hash": "abc123",
            "output_format": "json",
            "version": "v0.1.0"
        });
        assert!(validate_version_envelope(&v).is_empty());
    }

    #[test]
    fn compare_json_detects_value_mismatch() {
        let a = json!({"x": 1});
        let b = json!({"x": 2});
        let diffs = compare_json(&a, &b, "", None);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0].kind, DiffKind::ValueMismatch { .. }));
        assert_eq!(diffs[0].path, "x");
    }

    #[test]
    fn compare_json_detects_missing_field() {
        let a = json!({"x": 1, "y": 2});
        let b = json!({"x": 1});
        let diffs = compare_json(&a, &b, "", None);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0].kind, DiffKind::MissingField { .. }));
    }

    #[test]
    fn compare_json_detects_extra_field() {
        let a = json!({"x": 1});
        let b = json!({"x": 1, "y": 2});
        let diffs = compare_json(&a, &b, "", None);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0].kind, DiffKind::ExtraField { .. }));
    }

    #[test]
    fn compare_json_array_order_invariant() {
        let a = json!([{"id": "B", "v": 2}, {"id": "A", "v": 1}]);
        let b = json!([{"id": "A", "v": 1}, {"id": "B", "v": 2}]);
        // Without sort key: order matters
        let diffs_strict = compare_json(&a, &b, "", None);
        assert!(!diffs_strict.is_empty());
        // With sort key: order doesn't matter
        let diffs_sorted = compare_json(&a, &b, "", Some("id"));
        assert!(diffs_sorted.is_empty());
    }

    #[test]
    fn compare_json_ignoring_fields() {
        let a = json!({"x": 1, "ts": "2026-01-01"});
        let b = json!({"x": 1, "ts": "2026-03-04"});
        let diffs = compare_json_ignoring(&a, &b, "", &["ts"]);
        assert!(diffs.is_empty());
    }

    #[test]
    fn compare_json_nested_path() {
        let a = json!({"a": {"b": {"c": 1}}});
        let b = json!({"a": {"b": {"c": 2}}});
        let diffs = compare_json(&a, &b, "", None);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "a.b.c");
    }

    #[test]
    fn validate_type_at_checks_nested() {
        let v = json!({"a": {"b": [1, 2, 3]}});
        assert!(validate_type_at(&v, "a.b", JsonType::Array).is_empty());
        assert!(!validate_type_at(&v, "a.b", JsonType::String).is_empty());
        assert!(!validate_type_at(&v, "a.c", JsonType::String).is_empty());
    }

    #[test]
    fn resolve_path_handles_array_indices() {
        let v = json!({"items": [{"id": "A"}, {"id": "B"}]});
        assert_eq!(resolve_path(&v, "items.1.id"), Some(&json!("B")));
    }

    #[test]
    fn format_diffs_compact_output() {
        let diffs = vec![Diff {
            path: "a.b".to_string(),
            kind: DiffKind::ValueMismatch {
                expected: "1".to_string(),
                actual: "2".to_string(),
            },
        }];
        let output = format_diffs_compact(&diffs);
        assert!(output.contains("a.b"));
        assert!(output.contains("expected 1"));
    }

    #[test]
    fn array_length_mismatch_detected() {
        let a = json!([1, 2, 3]);
        let b = json!([1, 2]);
        let diffs = compare_json(&a, &b, "", None);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(
            diffs[0].kind,
            DiffKind::ArrayLengthMismatch {
                expected: 3,
                actual: 2
            }
        ));
    }

    #[test]
    fn type_mismatch_detected() {
        let a = json!({"x": "hello"});
        let b = json!({"x": 42});
        let diffs = compare_json(&a, &b, "", None);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0].kind, DiffKind::TypeMismatch { .. }));
    }
}
