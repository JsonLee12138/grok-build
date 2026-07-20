//! Resilient parsing for `[model.<id>]` TOML overrides.
//!
//! A model entry must survive a bad field: warn and skip the field, never
//! drop the model (managed configs must not lose catalog entries).
//!
//! Every table is deserialized through `serde_ignored`, so unknown fields
//! warn on every path and [`ConfigModelOverride`] stays the single source of
//! truth for the field set. When the whole-table parse fails, fields that
//! fail to parse on their own are pruned (one warning each) and the table is
//! parsed again. Non-table values are dropped with a warning.
//!
//! Warnings are retained on `Config::model_override_warnings` and surfaced by
//! `grok inspect`.

use indexmap::IndexMap;
use serde::Serialize;

use super::config::{ApiKeySource, ConfigModelOverride, ProviderConfig};

/// Category for a [`ModelOverrideWarning`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelOverrideWarningKind {
    /// Field name not recognized; field ignored.
    UnknownField,
    /// Value failed to parse; field skipped.
    InvalidValue,
    /// Legacy alias given alongside its canonical key; alias skipped.
    DuplicateAlias,
    /// Entry value is not a TOML table; entry dropped.
    NotATable,
    /// Entry failed to parse even after skipping invalid fields; the model
    /// keeps an empty override.
    UnparseableEntry,
}

/// One skipped field or dropped entry from `[model.*]` parsing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOverrideWarning {
    /// `None` when the warning is about the `[model]` section itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_key: Option<String>,
    /// `None` for warnings about the entry as a whole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub kind: ModelOverrideWarningKind,
    pub reason: String,
}

/// Result of [`parse_model_overrides`].
pub(crate) struct ParsedModelOverrides {
    pub models: IndexMap<String, ConfigModelOverride>,
    pub providers: IndexMap<String, ProviderConfig>,
    pub warnings: Vec<ModelOverrideWarning>,
}

/// Parses every `[model.<id>]` entry in `raw_config`, returning the overrides
/// and a warning for each skipped field or dropped entry.
pub(crate) fn parse_model_overrides(raw_config: &toml::Value) -> ParsedModelOverrides {
    let mut models = IndexMap::new();
    let mut warnings = Vec::new();
    let providers = parse_providers(raw_config, &mut warnings);
    let Some(section) = raw_config.get("model") else {
        return ParsedModelOverrides {
            models,
            providers,
            warnings,
        };
    };
    let Some(table) = section.as_table() else {
        warnings.push(ModelOverrideWarning {
            model_key: None,
            field: None,
            kind: ModelOverrideWarningKind::NotATable,
            reason: format!(
                "`model` must be a table of [model.<id>] entries, got {}; all model overrides ignored",
                section.type_str()
            ),
        });
        return ParsedModelOverrides {
            models,
            providers,
            warnings,
        };
    };
    for (model_key, value) in table {
        let Some(entry_table) = value.as_table() else {
            warnings.push(ModelOverrideWarning {
                model_key: Some(model_key.clone()),
                field: None,
                kind: ModelOverrideWarningKind::NotATable,
                reason: format!(
                    "expected a table like [model.\"{model_key}\"], got {}; entry dropped",
                    value.type_str()
                ),
            });
            continue;
        };
        let mut parsed_entry_table = entry_table.clone();
        let mut credentials_are_exclusive = false;
        if entry_table.contains_key("provider") {
            let credential =
                match parse_provider_model_credentials(model_key, entry_table, &mut warnings) {
                    Ok(credential) => credential,
                    Err(()) => continue,
                };
            if let Some(credential) = credential {
                credentials_are_exclusive = true;
                parsed_entry_table.remove("api_key");
                parsed_entry_table.remove("env_key");
                match credential {
                    ApiKeySource::Literal(value) => {
                        parsed_entry_table.insert("api_key".to_owned(), toml::Value::String(value));
                    }
                    ApiKeySource::Environment { env } => {
                        parsed_entry_table.insert("env_key".to_owned(), toml::Value::String(env));
                    }
                }
            }
        }
        let (mut entry, mut entry_warnings) =
            parse_model_override_table(model_key, parsed_entry_table);
        entry.credentials_are_exclusive = credentials_are_exclusive;
        if entry
            .alias
            .as_deref()
            .is_some_and(|alias| !is_valid_alias(alias))
        {
            entry.alias = None;
            entry_warnings.push(ModelOverrideWarning {
                model_key: Some(model_key.clone()),
                field: Some("alias".to_owned()),
                kind: ModelOverrideWarningKind::InvalidValue,
                reason: "alias must use non-empty ASCII letters, digits, '.', '_', or '-'; alias ignored"
                    .to_owned(),
            });
        }
        // Provider references have stricter fail-closed validation below. Drop
        // the generic field-level parse warning so an invalid non-string
        // reference produces one stable, value-free warning.
        if entry_table.get("provider").is_some_and(|v| !v.is_str()) {
            entry_warnings.retain(|warning| warning.field.as_deref() != Some("provider"));
        }
        warnings.extend(entry_warnings);
        if !normalize_provider_model(
            model_key,
            entry_table.get("provider"),
            &providers,
            &mut entry,
            &mut warnings,
        ) {
            continue;
        }
        models.insert(model_key.clone(), entry);
    }
    ParsedModelOverrides {
        models,
        providers,
        warnings,
    }
}

fn parse_providers(
    raw_config: &toml::Value,
    warnings: &mut Vec<ModelOverrideWarning>,
) -> IndexMap<String, ProviderConfig> {
    let mut providers = IndexMap::new();
    let Some(section) = raw_config.get("provider") else {
        return providers;
    };
    let Some(table) = section.as_table() else {
        warnings.push(ModelOverrideWarning {
            model_key: None,
            field: Some("provider".to_owned()),
            kind: ModelOverrideWarningKind::NotATable,
            reason: "`provider` must be a table of [provider.<id>] entries; all providers ignored"
                .to_owned(),
        });
        return providers;
    };
    for (provider_id, value) in table {
        if !is_valid_provider_id(provider_id) {
            warnings.push(ModelOverrideWarning {
                model_key: None,
                field: Some(format!("provider.{provider_id}")),
                kind: ModelOverrideWarningKind::InvalidValue,
                reason: "provider id must be a non-empty ASCII identifier containing only letters, digits, `.`, `_`, or `-`; provider ignored"
                    .to_owned(),
            });
            continue;
        }
        let Some(provider_table) = value.as_table() else {
            warnings.push(ModelOverrideWarning {
                model_key: None,
                field: Some(format!("provider.{provider_id}")),
                kind: ModelOverrideWarningKind::NotATable,
                reason: "provider definition must be a table; provider ignored".to_owned(),
            });
            continue;
        };
        let mut unknown = Vec::new();
        match serde_ignored::deserialize(toml::Value::Table(provider_table.clone()), |path| {
            unknown.push(path.to_string());
        }) {
            Ok(provider) if unknown.is_empty() => {
                let provider: ProviderConfig = provider;
                if provider
                    .api_key
                    .as_ref()
                    .is_some_and(|source| !source.is_valid())
                {
                    warnings.push(ModelOverrideWarning {
                        model_key: None,
                        field: Some(format!("provider.{provider_id}.api_key")),
                        kind: ModelOverrideWarningKind::InvalidValue,
                        reason: "provider api_key is invalid; provider ignored".to_owned(),
                    });
                } else {
                    providers.insert(provider_id.clone(), provider);
                }
            }
            Ok(_provider) => {
                for field in unknown {
                    warnings.push(ModelOverrideWarning {
                        model_key: None,
                        field: Some(format!("provider.{provider_id}.{field}")),
                        kind: ModelOverrideWarningKind::UnknownField,
                        reason: "unknown provider field; provider ignored".to_owned(),
                    });
                }
            }
            Err(_) => warnings.push(ModelOverrideWarning {
                model_key: None,
                field: Some(format!("provider.{provider_id}")),
                kind: ModelOverrideWarningKind::InvalidValue,
                // Do not include the deserializer error: future provider fields may
                // contain credentials, and warning text must never echo values.
                reason: "provider definition contains an invalid value; provider ignored"
                    .to_owned(),
            }),
        }
    }
    providers
}

fn is_valid_provider_id(provider_id: &str) -> bool {
    !provider_id.is_empty()
        && provider_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn parse_provider_model_credentials(
    model_key: &str,
    table: &toml::map::Map<String, toml::Value>,
    warnings: &mut Vec<ModelOverrideWarning>,
) -> Result<Option<ApiKeySource>, ()> {
    if table.contains_key("env_key") {
        warnings.push(ModelOverrideWarning {
            model_key: Some(model_key.to_owned()),
            field: Some("env_key".to_owned()),
            kind: ModelOverrideWarningKind::InvalidValue,
            reason: "provider model must use api_key union syntax; model entry dropped".to_owned(),
        });
        return Err(());
    }
    let Some(value) = table.get("api_key") else {
        return Ok(None);
    };
    let Ok(source) = value.clone().try_into::<ApiKeySource>() else {
        warnings.push(ModelOverrideWarning {
            model_key: Some(model_key.to_owned()),
            field: Some("api_key".to_owned()),
            kind: ModelOverrideWarningKind::InvalidValue,
            reason: "provider model api_key is invalid; model entry dropped".to_owned(),
        });
        return Err(());
    };
    if !source.is_valid() {
        warnings.push(ModelOverrideWarning {
            model_key: Some(model_key.to_owned()),
            field: Some("api_key".to_owned()),
            kind: ModelOverrideWarningKind::InvalidValue,
            reason: "provider model api_key is invalid; model entry dropped".to_owned(),
        });
        return Err(());
    }
    Ok(Some(source))
}

/// Validate the explicit provider reference and materialize provider-owned
/// identity/endpoint fields into the existing model override representation.
fn normalize_provider_model(
    model_key: &str,
    raw_provider: Option<&toml::Value>,
    providers: &IndexMap<String, ProviderConfig>,
    entry: &mut ConfigModelOverride,
    warnings: &mut Vec<ModelOverrideWarning>,
) -> bool {
    let Some(raw_provider) = raw_provider else {
        return true;
    };
    let Some(provider_id) = raw_provider.as_str() else {
        warnings.push(ModelOverrideWarning {
            model_key: Some(model_key.to_owned()),
            field: Some("provider".to_owned()),
            kind: ModelOverrideWarningKind::InvalidValue,
            reason: "provider reference must be a string; model entry dropped".to_owned(),
        });
        return false;
    };
    let Some(provider) = providers.get(provider_id) else {
        warnings.push(ModelOverrideWarning {
            model_key: Some(model_key.to_owned()),
            field: Some("provider".to_owned()),
            kind: ModelOverrideWarningKind::InvalidValue,
            reason: "referenced provider does not exist or is invalid; model entry dropped"
                .to_owned(),
        });
        return false;
    };
    let Some((key_provider, key_model_id)) = model_key.split_once('/') else {
        warnings.push(ModelOverrideWarning {
            model_key: Some(model_key.to_owned()),
            field: Some("provider".to_owned()),
            kind: ModelOverrideWarningKind::InvalidValue,
            reason:
                "provider model key must be `<provider-id>/<wire-model-id>`; model entry dropped"
                    .to_owned(),
        });
        return false;
    };
    if key_provider != provider_id || key_model_id.is_empty() {
        warnings.push(ModelOverrideWarning {
            model_key: Some(model_key.to_owned()),
            field: Some("provider".to_owned()),
            kind: ModelOverrideWarningKind::InvalidValue,
            reason: "provider model key does not match its provider or has no wire model id; model entry dropped"
                .to_owned(),
        });
        return false;
    }

    let changes_origin = model_changes_provider_origin(entry, provider);

    if entry.api_key.is_none() && entry.env_key.is_none() {
        if changes_origin {
            entry.clear_credentials = true;
        } else if let Some(api_key) = &provider.api_key {
            entry.credentials_are_exclusive = true;
            match api_key {
                ApiKeySource::Literal(value) => entry.api_key = Some(value.clone()),
                ApiKeySource::Environment { env } => {
                    entry.env_key = Some(super::config::EnvKeys::single(env));
                }
            }
        }
    }

    entry.provider = Some(provider_id.to_owned());
    if entry.model.is_none() {
        entry.model = Some(key_model_id.to_owned());
    }
    if entry.base_url.is_none() {
        entry.base_url.clone_from(&provider.base_url);
    }
    if entry.api_base_url.is_none() {
        entry.api_base_url.clone_from(&provider.api_base_url);
    }
    if entry.api_backend.is_none() {
        entry.api_backend.clone_from(&provider.api_backend);
    }
    if entry.auth_scheme.is_none() {
        entry.auth_scheme = if changes_origin {
            Some(xai_grok_sampler::AuthScheme::default())
        } else {
            provider.auth_scheme
        };
    }
    if entry.extra_headers.is_none() {
        if changes_origin {
            // ModelEntry has one shared header map for both endpoints. Once
            // either endpoint crosses origin, inherited provider headers must
            // be cleared from any prefetched/base entry as well.
            entry.extra_headers = Some(IndexMap::new());
        } else if !provider.extra_headers.is_empty() {
            entry.extra_headers = Some(provider.extra_headers.clone());
        }
    }
    true
}

fn model_changes_provider_origin(model: &ConfigModelOverride, provider: &ProviderConfig) -> bool {
    // Without a provider-owned session endpoint, the eventual base URL comes
    // from ModelEntry fallback/default state. Provider headers and auth cannot
    // be proven to belong to that origin, even if api_base_url is configured.
    let Some(provider_base_url) = provider.base_url.as_deref() else {
        return true;
    };
    let base_changed = model
        .base_url
        .as_deref()
        .is_some_and(|model_url| !urls_have_same_origin(model_url, provider_base_url));
    let provider_api_url = provider
        .api_base_url
        .as_deref()
        .unwrap_or(provider_base_url);
    let api_base_changed = model
        .api_base_url
        .as_deref()
        .is_some_and(|model_url| !urls_have_same_origin(model_url, provider_api_url));
    base_changed || api_base_changed
}

fn urls_have_same_origin(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let (Ok(left), Ok(right)) = (url::Url::parse(left), url::Url::parse(right)) else {
        return false;
    };
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

/// Logs the warnings when they differ from the previous parse, so a
/// persistently broken config logs once per process instead of once per parse.
pub(crate) fn log_model_override_warnings(warnings: &[ModelOverrideWarning]) {
    use std::hash::{Hash as _, Hasher as _};
    use std::sync::atomic::{AtomicU64, Ordering};

    static LAST_LOGGED: AtomicU64 = AtomicU64::new(0);
    // 0 means "no warnings"; real hashes are clamped to nonzero.
    let hash = if warnings.is_empty() {
        0
    } else {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        warnings.hash(&mut hasher);
        hasher.finish().max(1)
    };
    if LAST_LOGGED.swap(hash, Ordering::Relaxed) == hash {
        return;
    }

    for warning in warnings {
        tracing::warn!(
            model = warning.model_key.as_deref().unwrap_or("(section)"),
            field = warning.field.as_deref().unwrap_or("(entry)"),
            kind = ?warning.kind,
            reason = %warning.reason,
            "model_override: skipped invalid config"
        );
    }
    if !warnings.is_empty() {
        tracing::warn!(
            warnings = warnings.len(),
            "model_override: parsed with warnings; run `grok inspect` for details"
        );
    }
}

fn parse_model_override_table(
    model_key: &str,
    mut table: toml::map::Map<String, toml::Value>,
) -> (ConfigModelOverride, Vec<ModelOverrideWarning>) {
    let mut warnings = Vec::new();
    dedupe_aliases(model_key, &mut table, &mut warnings);

    // Unknown-field warnings come from whichever parse produces the returned
    // entry, so both paths report them identically.
    match deserialize_with_unknown_fields(table.clone()) {
        Ok((entry, unknown)) => {
            warnings.extend(unknown_field_warnings(model_key, unknown));
            (entry, warnings)
        }
        Err(_) => {
            prune_invalid_fields(model_key, &mut table, &mut warnings);
            match deserialize_with_unknown_fields(table) {
                Ok((entry, unknown)) => {
                    warnings.extend(unknown_field_warnings(model_key, unknown));
                    (entry, warnings)
                }
                Err(error) => {
                    // Reachable only when fields conflict jointly, e.g. an
                    // alias pair missing from `ALIASES`. Keep the model
                    // rather than dropping it.
                    warnings.push(ModelOverrideWarning {
                        model_key: Some(model_key.to_owned()),
                        field: None,
                        kind: ModelOverrideWarningKind::UnparseableEntry,
                        reason: format!(
                            "failed to parse after skipping invalid fields ({error}); using empty override"
                        ),
                    });
                    (ConfigModelOverride::default(), warnings)
                }
            }
        }
    }
}

/// `(canonical, legacy)` key pairs that serde rejects as duplicate fields
/// when both appear in one table. Keep in sync with the `#[serde(alias)]`
/// attributes on [`ConfigModelOverride`].
const ALIASES: &[(&str, &str)] = &[("compactions_remaining", "send_compactions_remaining")];

/// Removes one key of each [`ALIASES`] pair that appears twice in `table`.
/// The canonical key wins; when its value doesn't parse, the legacy key is
/// kept instead.
fn dedupe_aliases(
    model_key: &str,
    table: &mut toml::map::Map<String, toml::Value>,
    warnings: &mut Vec<ModelOverrideWarning>,
) {
    for &(canonical, legacy) in ALIASES {
        if !(table.contains_key(canonical) && table.contains_key(legacy)) {
            continue;
        }
        match field_parse_error(canonical, &table[canonical]) {
            None => {
                table.remove(legacy);
                warnings.push(ModelOverrideWarning {
                    model_key: Some(model_key.to_owned()),
                    field: Some(legacy.to_owned()),
                    kind: ModelOverrideWarningKind::DuplicateAlias,
                    reason: format!("legacy alias of {canonical}; skipped in favor of {canonical}"),
                });
            }
            Some(error) => {
                table.remove(canonical);
                warnings.push(ModelOverrideWarning {
                    model_key: Some(model_key.to_owned()),
                    field: Some(canonical.to_owned()),
                    kind: ModelOverrideWarningKind::InvalidValue,
                    reason: format!("{error}; skipped in favor of {legacy}"),
                });
            }
        }
    }
}

/// Deserializes `table`, also returning the unknown field names that serde
/// would otherwise silently discard.
fn deserialize_with_unknown_fields(
    table: toml::map::Map<String, toml::Value>,
) -> Result<(ConfigModelOverride, Vec<String>), toml::de::Error> {
    let mut unknown = Vec::new();
    let entry = serde_ignored::deserialize(toml::Value::Table(table), |path| {
        unknown.push(path.to_string());
    })?;
    Ok((entry, unknown))
}

fn unknown_field_warnings(model_key: &str, unknown: Vec<String>) -> Vec<ModelOverrideWarning> {
    unknown
        .into_iter()
        .map(|field| ModelOverrideWarning {
            model_key: Some(model_key.to_owned()),
            field: Some(field),
            kind: ModelOverrideWarningKind::UnknownField,
            reason: "unknown field".to_owned(),
        })
        .collect()
}

/// Removes each field that fails to parse on its own, one warning per field.
/// Unknown fields stay; the follow-up parse reports them.
fn prune_invalid_fields(
    model_key: &str,
    table: &mut toml::map::Map<String, toml::Value>,
    warnings: &mut Vec<ModelOverrideWarning>,
) {
    table.retain(|field, value| match field_parse_error(field, value) {
        None => true,
        Some(error) => {
            warnings.push(ModelOverrideWarning {
                model_key: Some(model_key.to_owned()),
                field: Some(field.to_owned()),
                kind: ModelOverrideWarningKind::InvalidValue,
                reason: error.to_string(),
            });
            false
        }
    });
}

/// Parses `field` in isolation, returning the error if it fails.
fn field_parse_error(field: &str, value: &toml::Value) -> Option<toml::de::Error> {
    let mut singleton = toml::map::Map::new();
    singleton.insert(field.to_owned(), value.clone());
    toml::Value::Table(singleton)
        .try_into::<ConfigModelOverride>()
        .err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampling::ApiBackend;
    use xai_grok_sampling_types::{
        CompactionAtTokens, CompactionsRemaining, ReasoningEffort, ReasoningEffortOption,
    };

    fn parse_cfg(toml_str: &str) -> crate::agent::config::Config {
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        crate::agent::config::Config::new_from_toml_cfg(&raw).expect("config should parse")
    }

    fn parse_raw(
        toml_str: &str,
    ) -> (
        IndexMap<String, ConfigModelOverride>,
        Vec<ModelOverrideWarning>,
    ) {
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        let ParsedModelOverrides {
            models, warnings, ..
        } = parse_model_overrides(&raw);
        (models, warnings)
    }

    #[test]
    fn provider_model_invalid_references_are_fail_closed() {
        let cases = [
            (
                "provider is not a string",
                "acme/demo-v1",
                r#"
                [provider.acme]
                base_url = "https://api.acme.example/v1"
                [model."acme/demo-v1"]
                provider = 7
                "#,
            ),
            (
                "provider does not exist",
                "acme/demo-v1",
                r#"
                [model."acme/demo-v1"]
                provider = "acme"
                "#,
            ),
            (
                "key has no separator",
                "demo-v1",
                r#"
                [provider.acme]
                base_url = "https://api.acme.example/v1"
                [model.demo-v1]
                provider = "acme"
                "#,
            ),
            (
                "key provider does not match",
                "other/demo-v1",
                r#"
                [provider.acme]
                base_url = "https://api.acme.example/v1"
                [model."other/demo-v1"]
                provider = "acme"
                "#,
            ),
            (
                "wire model id is empty",
                "acme/",
                r#"
                [provider.acme]
                base_url = "https://api.acme.example/v1"
                [model."acme/"]
                provider = "acme"
                "#,
            ),
        ];

        for (case, model_key, input) in cases {
            let (models, warnings) = parse_raw(input);
            assert!(
                !models.contains_key(model_key),
                "{case}: model must be dropped"
            );
            let matching: Vec<_> = warnings
                .iter()
                .filter(|warning| {
                    warning.kind == ModelOverrideWarningKind::InvalidValue
                        && warning.model_key.as_deref() == Some(model_key)
                        && warning.field.as_deref() == Some("provider")
                })
                .collect();
            assert_eq!(matching.len(), 1, "{case}: warning identity must be stable");
        }
    }

    #[test]
    fn provider_model_invalid_definitions_are_ignored_and_warnings_are_redacted() {
        let cases = [
            (
                "provider is not a table",
                "acme",
                "acme/demo-v1",
                ModelOverrideWarningKind::NotATable,
                r#"
                provider = { acme = "sensitive-provider-value" }
                [model."acme/demo-v1"]
                provider = "acme"
                "#,
                "sensitive-provider-value",
            ),
            (
                "provider id contains slash",
                "bad/id",
                "bad/id/demo-v1",
                ModelOverrideWarningKind::InvalidValue,
                r#"
                [provider."bad/id"]
                base_url = "https://api.example/v1"
                [model."bad/id/demo-v1"]
                provider = "bad/id"
                "#,
                "not-present-secret",
            ),
            (
                "provider id is whitespace",
                " ",
                " /demo-v1",
                ModelOverrideWarningKind::InvalidValue,
                r#"
                [provider." "]
                base_url = "sensitive-whitespace-value"
                [model." /demo-v1"]
                provider = " "
                "#,
                "sensitive-whitespace-value",
            ),
            (
                "provider id is unicode",
                "供应商",
                "供应商/demo-v1",
                ModelOverrideWarningKind::InvalidValue,
                r#"
                [provider."供应商"]
                base_url = "sensitive-unicode-value"
                [model."供应商/demo-v1"]
                provider = "供应商"
                "#,
                "sensitive-unicode-value",
            ),
            (
                "provider id contains another special character",
                "acme@prod",
                "acme@prod/demo-v1",
                ModelOverrideWarningKind::InvalidValue,
                r#"
                [provider."acme@prod"]
                base_url = "sensitive-special-value"
                [model."acme@prod/demo-v1"]
                provider = "acme@prod"
                "#,
                "sensitive-special-value",
            ),
            (
                "provider base_url has wrong type",
                "acme",
                "acme/demo-v1",
                ModelOverrideWarningKind::InvalidValue,
                r#"
                [provider.acme]
                base_url = 42
                [model."acme/demo-v1"]
                provider = "acme"
                "#,
                "42",
            ),
            (
                "provider has unknown field",
                "acme",
                "acme/demo-v1",
                ModelOverrideWarningKind::UnknownField,
                r#"
                [provider.acme]
                base_url = "https://api.acme.example/v1"
                secret_token = "plaintext-provider-secret"
                [model."acme/demo-v1"]
                provider = "acme"
                "#,
                "plaintext-provider-secret",
            ),
        ];

        for (case, provider_id, model_key, expected_kind, input, secret) in cases {
            let raw: toml::Value = toml::from_str(input).expect("case TOML parses");
            let ParsedModelOverrides {
                models,
                providers,
                warnings,
            } = parse_model_overrides(&raw);
            assert!(
                !providers.contains_key(provider_id),
                "{case}: provider must be ignored"
            );
            assert!(
                !models.contains_key(model_key),
                "{case}: referencing model must be ignored"
            );
            assert!(
                warnings.iter().any(|warning| warning.kind == expected_kind),
                "{case}: expected provider warning"
            );
            let rendered = serde_json::to_string(&warnings).expect("warnings serialize");
            assert!(
                !rendered.contains(secret),
                "{case}: warnings must not expose provider values"
            );
        }
    }

    #[test]
    fn provider_model_invalid_provider_api_keys_are_redacted() {
        let cases = [
            ("blank literal", r#"api_key = "   ""#, None),
            ("empty environment name", r#"api_key = { env = "" }"#, None),
            (
                "invalid environment name",
                r#"api_key = { env = "9INVALID_PROVIDER_SECRET" }"#,
                Some("9INVALID_PROVIDER_SECRET"),
            ),
            (
                "inline table has extra member",
                r#"api_key = { env = "VALID_ENV", extra = "provider-secret-extra-marker" }"#,
                Some("provider-secret-extra-marker"),
            ),
            ("api_key has wrong type", "api_key = 28405", Some("28405")),
        ];

        for (case, provider_api_key, secret) in cases {
            let raw: toml::Value = toml::from_str(&format!(
                r#"
                [provider.acme]
                base_url = "https://api.acme.example/v1"
                {provider_api_key}
                [model."acme/demo-v1"]
                provider = "acme"
                "#
            ))
            .expect("case TOML parses");
            let ParsedModelOverrides {
                models,
                providers,
                warnings,
            } = parse_model_overrides(&raw);
            assert!(!providers.contains_key("acme"), "{case}");
            assert!(!models.contains_key("acme/demo-v1"), "{case}");
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.kind == ModelOverrideWarningKind::InvalidValue),
                "{case}: invalid api_key must warn"
            );
            let rendered = serde_json::to_string(&warnings).unwrap();
            if let Some(secret) = secret {
                assert!(!rendered.contains(secret), "{case}: warning leaked a value");
            }
            assert!(
                !rendered.contains("api_key ="),
                "{case}: warning leaked TOML"
            );
        }
    }

    #[test]
    fn duplicate_compactions_keys_keeps_model() {
        let cfg = parse_cfg(
            r#"
            [model."grok-4.5"]
            model = "grok-4.5"
            env_key = "ANTHROPIC_AUTH_TOKEN"
            compactions_remaining = 1
            send_compactions_remaining = true
            "#,
        );
        let model = cfg
            .config_models
            .get("grok-4.5")
            .expect("grok-4.5 must remain in catalog");
        assert_eq!(
            model.compactions_remaining,
            Some(CompactionsRemaining::Fixed(1))
        );
        assert!(cfg.model_override_warnings.iter().any(|w| {
            w.kind == ModelOverrideWarningKind::DuplicateAlias
                && w.field.as_deref() == Some("send_compactions_remaining")
        }));
        let resolved = crate::agent::config::resolve_model_list(&cfg, None);
        assert!(resolved.contains_key("grok-4.5"));
    }

    #[test]
    fn legacy_alias_alone_parses_without_warning() {
        let cfg = parse_cfg(
            r#"
            [model."grok-4.5"]
            model = "grok-4.5"
            send_compactions_remaining = 2
            "#,
        );
        let model = cfg.config_models.get("grok-4.5").unwrap();
        assert_eq!(
            model.compactions_remaining,
            Some(CompactionsRemaining::Fixed(2))
        );
        assert!(cfg.model_override_warnings.is_empty());
    }

    #[test]
    fn invalid_reasoning_effort_skips_field_keeps_model() {
        let cfg = parse_cfg(
            r#"
            [model."grok-4.5"]
            model = "grok-4.5"
            env_key = "ANTHROPIC_AUTH_TOKEN"
            reasoning_effort = "not-a-level"
            "#,
        );
        let model = cfg
            .config_models
            .get("grok-4.5")
            .expect("grok-4.5 must remain in catalog");
        assert_eq!(model.model.as_deref(), Some("grok-4.5"));
        assert!(model.reasoning_effort.is_none());
        assert!(cfg.model_override_warnings.iter().any(|w| {
            w.kind == ModelOverrideWarningKind::InvalidValue
                && w.field.as_deref() == Some("reasoning_effort")
        }));
    }

    #[test]
    fn unknown_field_warns_but_keeps_known_fields() {
        let (models, warnings) = parse_raw(
            r#"
            [model."grok-4.5"]
            model = "grok-4.5"
            env_key = "TOKEN"
            future_field = 1
            "#,
        );
        let entry = models.get("grok-4.5").unwrap();
        assert_eq!(entry.model.as_deref(), Some("grok-4.5"));
        assert_eq!(
            entry.env_key.as_ref().and_then(|k| k.primary()),
            Some("TOKEN")
        );
        assert_eq!(
            warnings,
            vec![ModelOverrideWarning {
                model_key: Some("grok-4.5".to_owned()),
                field: Some("future_field".to_owned()),
                kind: ModelOverrideWarningKind::UnknownField,
                reason: "unknown field".to_owned(),
            }]
        );
    }

    /// An unknown field warns the same whether or not another field fails to
    /// parse.
    #[test]
    fn unknown_field_warning_is_path_independent() {
        let unknown_of = |toml_str: &str| {
            let (_, warnings) = parse_raw(toml_str);
            warnings
                .into_iter()
                .filter(|w| w.kind == ModelOverrideWarningKind::UnknownField)
                .collect::<Vec<_>>()
        };
        let fast = unknown_of(
            r#"
            [model.m]
            temprature = 0.5
            "#,
        );
        let slow = unknown_of(
            r#"
            [model.m]
            temprature = 0.5
            reasoning_effort = "not-a-level"
            "#,
        );
        assert_eq!(fast, slow);
        assert_eq!(fast.len(), 1);
        assert_eq!(fast[0].field.as_deref(), Some("temprature"));
    }

    #[test]
    fn prune_skips_invalid_fields_and_keeps_the_rest() {
        // A valid nested table survives an invalid sibling.
        let (models, warnings) = parse_raw(
            r#"
            [model.m]
            temperature = "hot"
            [model.m.extra_headers]
            x-team = "codegen"
            "#,
        );
        let entry = models.get("m").unwrap();
        assert_eq!(
            entry
                .extra_headers
                .as_ref()
                .and_then(|headers| headers.get("x-team"))
                .map(String::as_str),
            Some("codegen")
        );
        assert!(entry.temperature.is_none());
        assert!(warnings.iter().any(|w| {
            w.kind == ModelOverrideWarningKind::InvalidValue
                && w.field.as_deref() == Some("temperature")
        }));

        // All fields invalid: the model stays, with an empty override.
        let (models, warnings) = parse_raw(
            r#"
            [model.m]
            temperature = "hot"
            max_retries = "many"
            "#,
        );
        let entry = models.get("m").expect("model must remain in catalog");
        assert!(entry.temperature.is_none());
        assert!(entry.max_retries.is_none());
        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .all(|w| w.kind == ModelOverrideWarningKind::InvalidValue)
        );
    }

    #[test]
    fn invalid_canonical_key_falls_back_to_legacy_alias() {
        let (models, warnings) = parse_raw(
            r#"
            [model.m]
            compactions_remaining = "bad"
            send_compactions_remaining = 2
            "#,
        );
        let entry = models.get("m").unwrap();
        assert_eq!(
            entry.compactions_remaining,
            Some(CompactionsRemaining::Fixed(2))
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, ModelOverrideWarningKind::InvalidValue);
        assert_eq!(warnings[0].field.as_deref(), Some("compactions_remaining"));
    }

    #[test]
    fn non_table_model_section_warns_and_is_ignored() {
        let (models, warnings) = parse_raw(r#"model = "grok-4""#);
        assert!(models.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, ModelOverrideWarningKind::NotATable);
        assert_eq!(warnings[0].model_key, None);
        assert_eq!(warnings[0].field, None);
    }

    #[test]
    fn non_table_entry_is_dropped_with_warning() {
        let (models, warnings) = parse_raw(
            r#"
            [model]
            oops = 5
            "#,
        );
        assert!(models.is_empty(), "a scalar cannot define a model");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, ModelOverrideWarningKind::NotATable);
        assert_eq!(warnings[0].model_key.as_deref(), Some("oops"));
        assert_eq!(warnings[0].field, None);
    }

    /// Exhaustive literal (no `..`): a new struct field is a compile error
    /// here until the drift-guard tests cover it.
    fn fully_populated_override() -> ConfigModelOverride {
        ConfigModelOverride {
            provider: None,
            alias: Some("fast".into()),
            model: Some("m".into()),
            base_url: Some("https://example.com".into()),
            name: Some("Model M".into()),
            description: Some("desc".into()),
            api_key: Some("key".into()),
            env_key: Some(crate::agent::config::EnvKeys::single("ENV_KEY")),
            api_base_url: Some("https://api.example.com".into()),
            max_completion_tokens: Some(1024),
            temperature: Some(0.5),
            top_p: Some(0.9),
            api_backend: Some(ApiBackend::Messages),
            auth_scheme: Some(xai_grok_sampler::AuthScheme::XApiKey),
            extra_headers: Some(
                [("x-team".to_owned(), "codegen".to_owned())]
                    .into_iter()
                    .collect(),
            ),
            context_window: Some(200_000),
            auto_compact_threshold_percent: Some(80),
            system_prompt_label: Some("label".into()),
            use_concise: Some(true),
            agent_type: Some("agent".into()),
            inference_idle_timeout_secs: Some(60),
            max_retries: Some(3),
            hidden: Some(false),
            supported_in_api: Some(true),
            reasoning_effort: Some(ReasoningEffort::High),
            supports_reasoning_effort: Some(true),
            reasoning_efforts: vec![ReasoningEffortOption {
                id: "deep".to_string(),
                value: ReasoningEffort::High,
                label: "Deep".to_string(),
                description: Some("Deep reasoning".to_string()),
                default: true,
            }],
            supports_backend_search: Some(false),
            compactions_remaining: Some(CompactionsRemaining::Fixed(1)),
            compaction_at_tokens: Some(CompactionAtTokens::Fixed(100_000)),
            show_model_fingerprint: Some(true),
            stream_tool_calls: Some(false),
            clear_credentials: false,
            credentials_are_exclusive: false,
        }
    }

    fn parse_single_entry(
        entry: toml::map::Map<String, toml::Value>,
    ) -> (
        IndexMap<String, ConfigModelOverride>,
        Vec<ModelOverrideWarning>,
    ) {
        let mut model_table = toml::map::Map::new();
        model_table.insert("m".to_owned(), toml::Value::Table(entry));
        let mut root = toml::map::Map::new();
        root.insert("model".to_owned(), toml::Value::Table(model_table));
        let ParsedModelOverrides {
            models, warnings, ..
        } = parse_model_overrides(&toml::Value::Table(root));
        (models, warnings)
    }

    #[test]
    fn fully_populated_override_round_trips_without_warnings() {
        let serialized = toml::Value::try_from(fully_populated_override()).unwrap();
        let (models, warnings) = parse_single_entry(serialized.as_table().unwrap().clone());
        assert_eq!(warnings, Vec::new(), "no field may be skipped or unknown");
        let reparsed = toml::Value::try_from(models.get("m").unwrap()).unwrap();
        assert_eq!(reparsed, serialized, "round-trip must be lossless");
    }

    /// Drift guard: every `#[serde(alias)]` on [`ConfigModelOverride`] must
    /// have a matching `ALIASES` pair, and vice versa. An unregistered alias
    /// would send both-keys configs to the empty-override fallback.
    #[test]
    fn every_struct_alias_is_registered_in_aliases() {
        let source = include_str!("config.rs");
        let start = source
            .find("pub struct ConfigModelOverride {")
            .expect("ConfigModelOverride definition in config.rs");
        let block = &source[start..];
        let block = &block[..block.find("\n}").expect("struct end")];

        let mut found = Vec::new();
        let mut rest = block;
        while let Some(pos) = rest.find("#[serde(alias = \"") {
            let after = &rest[pos + "#[serde(alias = \"".len()..];
            let legacy = &after[..after.find('"').expect("closing quote")];
            let field = &after[after.find("pub ").expect("field after alias") + 4..];
            let canonical = &field[..field.find(':').expect("field type colon")];
            found.push((canonical.to_owned(), legacy.to_owned()));
            rest = after;
        }
        assert_eq!(
            block.matches("alias").count(),
            found.len(),
            "an alias on ConfigModelOverride was not recognized; write it as \
             `#[serde(alias = \"...\")]` on its own line, or update this scan"
        );
        found.sort();

        let mut registered: Vec<(String, String)> = ALIASES
            .iter()
            .map(|&(c, l)| (c.to_owned(), l.to_owned()))
            .collect();
        registered.sort();
        assert_eq!(
            found, registered,
            "#[serde(alias)] attributes on ConfigModelOverride and ALIASES must match"
        );
    }

    /// Drift guard for `ALIASES`, in both directions: every pair must be a
    /// real serde alias (a both-keys table fails a plain parse), and the
    /// parser must resolve it to the canonical key with a single warning.
    #[test]
    fn every_aliases_pair_is_a_real_serde_alias_and_dedupes() {
        let reference = toml::Value::try_from(fully_populated_override()).unwrap();
        for &(canonical, legacy) in ALIASES {
            let value = reference
                .get(canonical)
                .unwrap_or_else(|| panic!("{canonical} missing from fully_populated_override"));
            let mut entry = toml::map::Map::new();
            entry.insert(canonical.to_owned(), value.clone());
            entry.insert(legacy.to_owned(), value.clone());
            assert!(
                toml::Value::Table(entry.clone())
                    .try_into::<ConfigModelOverride>()
                    .is_err(),
                "{canonical}/{legacy} is not a serde alias pair; remove it from ALIASES"
            );

            let (models, warnings) = parse_single_entry(entry);
            let parsed = toml::Value::try_from(models.get("m").unwrap()).unwrap();
            assert_eq!(
                parsed.get(canonical),
                Some(value),
                "canonical value must be retained"
            );
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].kind, ModelOverrideWarningKind::DuplicateAlias);
            assert_eq!(warnings[0].field.as_deref(), Some(legacy));
        }
    }
}
