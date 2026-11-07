use crate::bazel::label::CanonicalLabel;
use crate::bazel::rule::{DirectAttributeValue, Rule};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigurationKind {
    Target,
    Exec,
    Null,
}

/// Namespaces distinguish native options, --define entries, and Starlark settings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigurationKey {
    Native(String),
    Define(String),
    BuildSetting(CanonicalLabel<'static>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConfigurationValue {
    String(String),
    Label(CanonicalLabel<'static>),
    List(Vec<ConfigurationValue>),
}

impl ConfigurationValue {
    fn matches(&self, expected: &str) -> bool {
        match self {
            Self::String(value) => value == expected,
            Self::Label(label) => label.to_string() == expected,
            Self::List(values) => values.iter().any(|value| value.matches(expected)),
        }
    }
}

/// An immutable option map. Transitions retain only overrides and removals;
/// unchanged options are borrowed from the shared parent configuration.
#[derive(Debug)]
pub struct BuildConfiguration {
    parent: Option<Arc<BuildConfiguration>>,
    options: BTreeMap<ConfigurationKey, Option<ConfigurationValue>>,
    checksum: OnceLock<String>,
}

impl BuildConfiguration {
    pub fn new(options: impl IntoIterator<Item = (ConfigurationKey, ConfigurationValue)>) -> Self {
        Self {
            parent: None,
            options: options
                .into_iter()
                .map(|(key, value)| (key, Some(value)))
                .collect(),
            checksum: OnceLock::new(),
        }
    }

    pub fn transition(
        self: &Arc<Self>,
        overrides: impl IntoIterator<Item = (ConfigurationKey, Option<ConfigurationValue>)>,
    ) -> Self {
        Self {
            parent: Some(self.clone()),
            options: overrides.into_iter().collect(),
            checksum: OnceLock::new(),
        }
    }

    pub fn get(&self, key: &ConfigurationKey) -> Option<&ConfigurationValue> {
        let mut current = Some(self);
        while let Some(configuration) = current {
            if let Some(value) = configuration.options.get(key) {
                return value.as_ref();
            }
            current = configuration.parent.as_deref();
        }
        None
    }

    pub fn matches_options(&self, requirements: &ConfigurationRequirements) -> bool {
        requirements.values.iter().all(|(name, expected)| {
            self.get(&ConfigurationKey::Native(name.clone()))
                .is_some_and(|actual| actual.matches(expected))
        }) && requirements.define_values.iter().all(|(name, expected)| {
            self.get(&ConfigurationKey::Define(name.clone()))
                .is_some_and(|actual| actual.matches(expected))
        }) && requirements.flag_values.iter().all(|(label, expected)| {
            self.get(&ConfigurationKey::BuildSetting(label.clone()))
                .is_some_and(|actual| actual.matches(expected))
        })
    }

    pub fn checksum(&self) -> &str {
        self.checksum.get_or_init(|| {
            let mut options = BTreeMap::new();
            let mut current = Some(self);
            while let Some(configuration) = current {
                for (key, value) in &configuration.options {
                    options.entry(key).or_insert(value.as_ref());
                }
                current = configuration.parent.as_deref();
            }
            // Identity is determined by effective values, not the transition path.
            let options = options
                .into_iter()
                .filter_map(|(key, value)| value.map(|value| (key, value)))
                .collect::<BTreeMap<_, _>>();
            let mut hasher = DefaultHasher::new();
            options.hash(&mut hasher);
            format!("{:07x}", hasher.finish() & 0x0fff_ffff)
        })
    }
}

pub fn parse_bool(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" => Ok(true),
        "0" | "false" | "f" | "no" | "n" => Ok(false),
        _ => Err(format!("invalid boolean value `{value}`")),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigurationRequirements {
    pub values: BTreeMap<String, String>,
    pub define_values: BTreeMap<String, String>,
    pub flag_values: BTreeMap<CanonicalLabel<'static>, String>,
    pub constraints: BTreeSet<CanonicalLabel<'static>>,
}

impl ConfigurationRequirements {
    pub fn from_rule(rule: &Rule) -> anyhow::Result<Self> {
        if rule.rule_class().native_name() != Some("config_setting") {
            anyhow::bail!("target is not a config_setting");
        }
        let mut values = BTreeMap::new();
        let mut define_values = string_dict(rule, "define_values")?;
        for (name, value) in string_dict(rule, "values")? {
            if name == "define" {
                let (key, value) = value.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!("config_setting define value must have the form NAME=VALUE")
                })?;
                if let Some(existing) = define_values.insert(key.to_owned(), value.to_owned())
                    && existing != value
                {
                    anyhow::bail!("config_setting has conflicting values for define `{key}`");
                }
            } else {
                values.insert(name.clone(), normalize_option_value(&name, &value)?);
            }
        }
        let flag_values = label_keyed_string_dict(rule, "flag_values")?;
        if !flag_values.is_empty() {
            anyhow::bail!("config_setting flag_values are not supported by cquery yet");
        }
        let requirements = Self {
            values,
            define_values,
            flag_values,
            constraints: label_list(rule, "constraint_values")?.into_iter().collect(),
        };
        if requirements.values.is_empty()
            && requirements.define_values.is_empty()
            && requirements.flag_values.is_empty()
            && requirements.constraints.is_empty()
        {
            anyhow::bail!("config_setting must specify at least one condition");
        }
        Ok(requirements)
    }

    pub fn for_constraint(label: CanonicalLabel<'static>) -> Self {
        Self {
            constraints: BTreeSet::from([label]),
            ..Self::default()
        }
    }

    pub fn is_strict_specialization_of(&self, other: &Self) -> bool {
        self != other
            && map_contains(&self.values, &other.values)
            && map_contains(&self.define_values, &other.define_values)
            && map_contains(&self.flag_values, &other.flag_values)
            && self.constraints.is_superset(&other.constraints)
    }
}

fn normalize_option_value(name: &str, value: &str) -> anyhow::Result<String> {
    match name {
        "cpu" | "platforms" => Ok(value.to_owned()),
        "compilation_mode" if matches!(value, "fastbuild" | "dbg" | "opt") => Ok(value.to_owned()),
        "compilation_mode" => anyhow::bail!("invalid compilation_mode value `{value}`"),
        "force_pic" => parse_bool(value)
            .map(|value| value.to_string())
            .map_err(|_| anyhow::anyhow!("invalid force_pic value `{value}`")),
        _ => anyhow::bail!("config_setting option `{name}` is not supported by cquery yet"),
    }
}

fn map_contains<K: Ord, V: PartialEq>(left: &BTreeMap<K, V>, right: &BTreeMap<K, V>) -> bool {
    right
        .iter()
        .all(|(key, value)| left.get(key).is_some_and(|candidate| candidate == value))
}

fn direct_attribute<'a>(
    rule: &'a Rule,
    name: &str,
) -> anyhow::Result<Option<&'a DirectAttributeValue>> {
    rule.attribute(name)
        .map(|value| {
            value
                .direct_value()
                .ok_or_else(|| anyhow::anyhow!("config_setting attribute `{name}` is configurable"))
        })
        .transpose()
}

fn string_dict(rule: &Rule, name: &str) -> anyhow::Result<BTreeMap<String, String>> {
    match direct_attribute(rule, name)? {
        Some(DirectAttributeValue::StringDict(values)) => Ok(values.clone()),
        Some(_) => anyhow::bail!("config_setting attribute `{name}` has the wrong type"),
        None => Ok(BTreeMap::new()),
    }
}

fn label_keyed_string_dict(
    rule: &Rule,
    name: &str,
) -> anyhow::Result<BTreeMap<CanonicalLabel<'static>, String>> {
    match direct_attribute(rule, name)? {
        Some(DirectAttributeValue::LabelKeyedStringDict(values)) => Ok(values.clone()),
        Some(_) => anyhow::bail!("config_setting attribute `{name}` has the wrong type"),
        None => Ok(BTreeMap::new()),
    }
}

pub fn label_list(rule: &Rule, name: &str) -> anyhow::Result<Vec<CanonicalLabel<'static>>> {
    match direct_attribute(rule, name)? {
        Some(DirectAttributeValue::LabelList(values)) => Ok(values.clone()),
        Some(_) => anyhow::bail!("attribute `{name}` has the wrong type"),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_options_use_bazel_spellings() {
        for value in ["1", "T", "yes", "true"] {
            assert_eq!(normalize_option_value("force_pic", value).unwrap(), "true");
        }
        for value in ["0", "n", "false", "NO"] {
            assert_eq!(normalize_option_value("force_pic", value).unwrap(), "false");
        }
    }

    #[test]
    fn sparse_transitions_inherit_override_and_remove_options() {
        let cpu = ConfigurationKey::Native("cpu".into());
        let setting = ConfigurationKey::BuildSetting(CanonicalLabel::new(
            crate::bazel::label::MAIN_REPO,
            "flags",
            "mode",
        ));
        let define = ConfigurationKey::Define("feature".into());
        let root = Arc::new(BuildConfiguration::new([
            (cpu.clone(), ConfigurationValue::String("arm".into())),
            (setting.clone(), ConfigurationValue::String("custom".into())),
            (define.clone(), ConfigurationValue::String("on".into())),
        ]));
        let child = Arc::new(
            root.transition([(cpu.clone(), Some(ConfigurationValue::String("x86".into())))]),
        );
        assert_eq!(child.options.len(), 1);
        assert!(Arc::ptr_eq(child.parent.as_ref().unwrap(), &root));
        assert!(std::ptr::eq(
            child.get(&setting).unwrap(),
            root.get(&setting).unwrap()
        ));
        assert_eq!(
            root.get(&cpu),
            Some(&ConfigurationValue::String("arm".into()))
        );
        let grandchild = child.transition([(define.clone(), None)]);
        assert!(grandchild.get(&define).is_none());
        assert!(root.get(&define).is_some());

        let flat = BuildConfiguration::new([
            (cpu, ConfigurationValue::String("x86".into())),
            (setting, ConfigurationValue::String("custom".into())),
        ]);
        assert_eq!(flat.checksum(), grandchild.checksum());
        assert_ne!(root.checksum(), child.checksum());
        assert_eq!(root.transition([]).checksum(), root.checksum());
    }

    #[test]
    fn options_support_arbitrary_native_and_label_keys_and_values() {
        let setting = CanonicalLabel::new(crate::bazel::label::MAIN_REPO, "flags", "label");
        let value = CanonicalLabel::new(crate::bazel::label::MAIN_REPO, "lib", "chosen");
        let configuration = BuildConfiguration::new([
            (
                ConfigurationKey::BuildSetting(setting.clone()),
                ConfigurationValue::Label(value.clone()),
            ),
            (
                ConfigurationKey::Native("copt".into()),
                ConfigurationValue::List(vec![
                    ConfigurationValue::String("-Wall".into()),
                    ConfigurationValue::String("-O2".into()),
                ]),
            ),
        ]);
        assert!(configuration.matches_options(&ConfigurationRequirements {
            values: BTreeMap::from([("copt".into(), "-Wall".into())]),
            flag_values: BTreeMap::from([(setting, value.to_string())]),
            ..ConfigurationRequirements::default()
        }));
    }

    #[test]
    fn option_values_are_normalized_for_specialization() {
        assert_eq!(normalize_option_value("force_pic", "1").unwrap(), "true");
        assert!(normalize_option_value("force_pic", "maybe").is_err());
        assert!(normalize_option_value("unknown", "value").is_err());
    }

    #[test]
    fn specialization_requires_a_strict_superset() {
        let basic = ConfigurationRequirements {
            values: BTreeMap::from([("cpu".to_owned(), "arm".to_owned())]),
            ..ConfigurationRequirements::default()
        };
        let specialized = ConfigurationRequirements {
            values: BTreeMap::from([
                ("compilation_mode".to_owned(), "dbg".to_owned()),
                ("cpu".to_owned(), "arm".to_owned()),
            ]),
            ..ConfigurationRequirements::default()
        };
        assert!(specialized.is_strict_specialization_of(&basic));
        assert!(!basic.is_strict_specialization_of(&specialized));
        assert!(!basic.is_strict_specialization_of(&basic));
    }
}
