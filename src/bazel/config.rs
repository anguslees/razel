use crate::bazel::label::CanonicalLabel;
use crate::bazel::rule::{DirectAttributeValue, Rule};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigurationKind {
    Target,
    Exec,
    Null,
}

/// Namespaces distinguish native options, --define entries, and Starlark settings. A key can
/// borrow its name, so looking up an option doesn't allocate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigurationKey<'a> {
    Native(Cow<'a, str>),
    Define(Cow<'a, str>),
    BuildSetting(CanonicalLabel<'a>),
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
    options: BTreeMap<ConfigurationKey<'static>, Option<ConfigurationValue>>,
    checksum: OnceLock<String>,
}

impl BuildConfiguration {
    pub fn new(
        options: impl IntoIterator<Item = (ConfigurationKey<'static>, ConfigurationValue)>,
    ) -> Self {
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
        overrides: impl IntoIterator<Item = (ConfigurationKey<'static>, Option<ConfigurationValue>)>,
    ) -> Self {
        Self {
            parent: Some(self.clone()),
            options: overrides.into_iter().collect(),
            checksum: OnceLock::new(),
        }
    }

    /// The effective value of option `key`. The key may borrow its name, so a lookup doesn't
    /// allocate; the value is then borrowed for no longer than the key.
    pub fn get<'s>(&'s self, key: &ConfigurationKey<'s>) -> Option<&'s ConfigurationValue> {
        let mut current = Some(self);
        while let Some(configuration) = current {
            if let Some(value) = configuration.options.get(key) {
                return value.as_ref();
            }
            current = configuration.parent.as_deref();
        }
        None
    }

    pub fn matches_options(&self, requirements: &ConfigurationRequirements<'_>) -> bool {
        // The keys borrow the requirements' names, so checking doesn't allocate.
        requirements.values.iter().all(|(name, expected)| {
            self.get(&ConfigurationKey::Native(Cow::Borrowed(*name)))
                .is_some_and(|actual| actual.matches(expected))
        }) && requirements.define_values.iter().all(|(name, expected)| {
            self.get(&ConfigurationKey::Define(Cow::Borrowed(*name)))
                .is_some_and(|actual| actual.matches(expected))
        }) && requirements.flag_values.iter().all(|(label, expected)| {
            self.get(&ConfigurationKey::BuildSetting(label.as_borrowed()))
                .is_some_and(|actual| actual.matches(expected))
        })
    }

    /// A short hash of the effective options. Configurations with the same effective options have
    /// the same checksum, whichever transitions produced them.
    pub fn checksum(&self) -> &str {
        self.checksum.get_or_init(|| {
            // Walk from this configuration up through its parents, keeping the first entry found
            // for each option, so overrides win. A `None` entry is an option that a transition
            // removed: it hides any parent value, and is skipped when hashing.
            let mut options =
                BTreeMap::<&ConfigurationKey<'static>, Option<&ConfigurationValue>>::new();
            let mut current = Some(self);
            while let Some(configuration) = current {
                for (key, value) in &configuration.options {
                    options.entry(key).or_insert(value.as_ref());
                }
                current = configuration.parent.as_deref();
            }
            let mut hasher = DefaultHasher::new();
            for (key, value) in &options {
                if let Some(value) = value {
                    key.hash(&mut hasher);
                    value.hash(&mut hasher);
                }
            }
            format!("{:07x}", hasher.finish() & 0x0fff_ffff)
        })
    }
}

/// Parses Bazel's boolean spellings, ignoring ASCII case.
pub fn parse_bool(value: &str) -> Result<bool, String> {
    let spelled = |spellings: [&str; 5]| {
        spellings
            .iter()
            .any(|spelling| value.eq_ignore_ascii_case(spelling))
    };
    if spelled(["1", "true", "t", "yes", "y"]) {
        Ok(true)
    } else if spelled(["0", "false", "f", "no", "n"]) {
        Ok(false)
    } else {
        Err(format!("invalid boolean value `{value}`"))
    }
}

/// What a select() condition requires of a configuration. Requirements borrow from the
/// condition's rule, so checking a condition copies no strings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigurationRequirements<'a> {
    pub values: BTreeMap<&'a str, &'a str>,
    pub define_values: BTreeMap<&'a str, &'a str>,
    pub flag_values: BTreeMap<CanonicalLabel<'a>, &'a str>,
    pub constraints: BTreeSet<CanonicalLabel<'a>>,
}

impl<'a> ConfigurationRequirements<'a> {
    pub fn from_rule(rule: &'a Rule) -> anyhow::Result<Self> {
        if rule.rule_class().native_name() != Some("config_setting") {
            anyhow::bail!("target is not a config_setting");
        }
        let mut values = BTreeMap::new();
        let mut define_values = string_dict(rule, "define_values")?
            .into_iter()
            .flatten()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<BTreeMap<_, _>>();
        for (name, value) in string_dict(rule, "values")?.into_iter().flatten() {
            if name == "define" {
                let (key, value) = value.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!("config_setting define value must have the form NAME=VALUE")
                })?;
                if let Some(existing) = define_values.insert(key, value)
                    && existing != value
                {
                    anyhow::bail!("config_setting has conflicting values for define `{key}`");
                }
            } else {
                values.insert(name.as_str(), normalize_option_value(name, value)?);
            }
        }
        let flag_values = label_keyed_string_dict(rule, "flag_values")?
            .into_iter()
            .flatten()
            .map(|(label, value)| (label.as_borrowed(), value.as_str()))
            .collect::<BTreeMap<_, _>>();
        if !flag_values.is_empty() {
            anyhow::bail!("config_setting flag_values are not supported by cquery yet");
        }
        let requirements = Self {
            values,
            define_values,
            flag_values,
            constraints: label_list(rule, "constraint_values")?
                .iter()
                .map(CanonicalLabel::as_borrowed)
                .collect(),
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

    pub fn for_constraint(label: CanonicalLabel<'a>) -> Self {
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

/// Normalizes an option value, so equivalent spellings compare equal.
fn normalize_option_value<'a>(name: &str, value: &'a str) -> anyhow::Result<&'a str> {
    match name {
        "cpu" | "platforms" => Ok(value),
        "compilation_mode" if matches!(value, "fastbuild" | "dbg" | "opt") => Ok(value),
        "compilation_mode" => anyhow::bail!("invalid compilation_mode value `{value}`"),
        "force_pic" => match parse_bool(value) {
            Ok(true) => Ok("true"),
            Ok(false) => Ok("false"),
            Err(_) => anyhow::bail!("invalid force_pic value `{value}`"),
        },
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

fn string_dict<'a>(
    rule: &'a Rule,
    name: &str,
) -> anyhow::Result<Option<&'a BTreeMap<String, String>>> {
    match direct_attribute(rule, name)? {
        Some(DirectAttributeValue::StringDict(values)) => Ok(Some(values)),
        Some(_) => anyhow::bail!("config_setting attribute `{name}` has the wrong type"),
        None => Ok(None),
    }
}

fn label_keyed_string_dict<'a>(
    rule: &'a Rule,
    name: &str,
) -> anyhow::Result<Option<&'a BTreeMap<CanonicalLabel<'static>, String>>> {
    match direct_attribute(rule, name)? {
        Some(DirectAttributeValue::LabelKeyedStringDict(values)) => Ok(Some(values)),
        Some(_) => anyhow::bail!("config_setting attribute `{name}` has the wrong type"),
        None => Ok(None),
    }
}

pub fn label_list<'a>(rule: &'a Rule, name: &str) -> anyhow::Result<&'a [CanonicalLabel<'static>]> {
    match direct_attribute(rule, name)? {
        Some(DirectAttributeValue::LabelList(values)) => Ok(values),
        Some(_) => anyhow::bail!("attribute `{name}` has the wrong type"),
        None => Ok(&[]),
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
        // Like Bazel, only whole spellings are accepted: no prefixes, padding or other numbers.
        for value in ["", "maybe", "2", "tru", "yess", " true", "false "] {
            let error = normalize_option_value("force_pic", value).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("invalid force_pic value `{value}`")
            );
            assert_eq!(
                parse_bool(value),
                Err(format!("invalid boolean value `{value}`"))
            );
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
        let value = value.to_string();
        assert!(configuration.matches_options(&ConfigurationRequirements {
            values: BTreeMap::from([("copt", "-Wall")]),
            flag_values: BTreeMap::from([(setting.as_borrowed(), value.as_str())]),
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
            values: BTreeMap::from([("cpu", "arm")]),
            ..ConfigurationRequirements::default()
        };
        let specialized = ConfigurationRequirements {
            values: BTreeMap::from([("compilation_mode", "dbg"), ("cpu", "arm")]),
            ..ConfigurationRequirements::default()
        };
        assert!(specialized.is_strict_specialization_of(&basic));
        assert!(!basic.is_strict_specialization_of(&specialized));
        assert!(!basic.is_strict_specialization_of(&basic));
    }
}
