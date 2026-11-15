#![allow(dead_code)]

use crate::bazel::config::ConfigurationRequirements;
use crate::bazel::label::CanonicalLabel;
use starlark::values::OwnedFrozenValue;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RuleClassId {
    Native(String),
    Starlark {
        defining_bzl: CanonicalLabel<'static>,
        exported_name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleClassKind {
    Ordinary,
    Executable,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeType {
    Bool,
    Int,
    IntList,
    String,
    StringList,
    StringListDict,
    StringDict,
    Label,
    LabelList,
    LabelKeyedStringDict,
    LabelListDict,
    StringKeyedLabelDict,
    Output,
    OutputList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeDependency {
    Dependency,
    NoDependency,
    Output,
}

/// The configuration that an attribute's dependencies are built in, from its `cfg`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AttributeTransition {
    /// The configuration of the depending target: Bazel's default, `cfg = "target"`.
    #[default]
    Target,
    /// The execution platform's configuration: `cfg = "exec"`.
    Exec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectAttributeValue {
    None,
    Bool(bool),
    Int(i64),
    IntList(Vec<i64>),
    String(String),
    StringList(Vec<String>),
    StringListDict(BTreeMap<String, Vec<String>>),
    StringDict(BTreeMap<String, String>),
    Label(CanonicalLabel<'static>),
    LabelList(Vec<CanonicalLabel<'static>>),
    LabelKeyedStringDict(BTreeMap<CanonicalLabel<'static>, String>),
    LabelListDict(BTreeMap<String, Vec<CanonicalLabel<'static>>>),
    StringKeyedLabelDict(BTreeMap<String, CanonicalLabel<'static>>),
    Output(CanonicalLabel<'static>),
    OutputList(Vec<CanonicalLabel<'static>>),
}

impl DirectAttributeValue {
    pub fn visit_labels<'a>(&'a self, mut visitor: impl FnMut(&'a CanonicalLabel<'static>)) {
        self.try_visit_labels::<std::convert::Infallible>(|label| {
            visitor(label);
            Ok(())
        })
        .unwrap();
    }

    pub fn try_visit_labels<'a, E>(
        &'a self,
        mut visitor: impl FnMut(&'a CanonicalLabel<'static>) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Label(label) | Self::Output(label) => visitor(label)?,
            Self::LabelList(labels) | Self::OutputList(labels) => {
                for label in labels {
                    visitor(label)?;
                }
            }
            Self::LabelListDict(labels) => {
                for label in labels.values().flatten() {
                    visitor(label)?;
                }
            }
            Self::LabelKeyedStringDict(labels) => {
                for label in labels.keys() {
                    visitor(label)?;
                }
            }
            Self::StringKeyedLabelDict(labels) => {
                for label in labels.values() {
                    visitor(label)?;
                }
            }
            Self::None
            | Self::Bool(_)
            | Self::Int(_)
            | Self::IntList(_)
            | Self::String(_)
            | Self::StringList(_)
            | Self::StringListDict(_)
            | Self::StringDict(_) => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub entries: BTreeMap<CanonicalLabel<'static>, Option<DirectAttributeValue>>,
    pub default: Option<Option<DirectAttributeValue>>,
    pub no_match_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeValuePart {
    Direct(DirectAttributeValue),
    Selector(Selector),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeValue {
    pub parts: Vec<AttributeValuePart>,
}

impl AttributeValue {
    pub fn direct(value: DirectAttributeValue) -> Self {
        Self {
            parts: vec![AttributeValuePart::Direct(value)],
        }
    }

    pub fn direct_value(&self) -> Option<&DirectAttributeValue> {
        match self.parts.as_slice() {
            [AttributeValuePart::Direct(value)] => Some(value),
            _ => None,
        }
    }

    pub fn into_direct_value(mut self) -> Option<DirectAttributeValue> {
        if self.parts.len() != 1 {
            return None;
        }
        match self.parts.pop() {
            Some(AttributeValuePart::Direct(value)) => Some(value),
            Some(AttributeValuePart::Selector(_)) | None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeSchema {
    pub name: String,
    pub public: bool,
    pub attribute_type: AttributeType,
    pub default: Option<DirectAttributeValue>,
    pub mandatory: bool,
    pub dependency: AttributeDependency,
    pub configurable: bool,
    pub selector_conditions_are_dependencies: bool,
    pub allow_files: Option<bool>,
    pub allow_empty: Option<bool>,
    pub transition: AttributeTransition,
}

impl AttributeSchema {
    pub fn new(
        name: impl Into<String>,
        attribute_type: AttributeType,
        default: Option<DirectAttributeValue>,
        dependency: AttributeDependency,
    ) -> Self {
        let configurable = !matches!(
            attribute_type,
            AttributeType::Output | AttributeType::OutputList
        );
        Self {
            name: name.into(),
            public: true,
            attribute_type,
            default,
            mandatory: false,
            dependency,
            configurable,
            selector_conditions_are_dependencies: configurable,
            allow_files: None,
            allow_empty: None,
            transition: AttributeTransition::Target,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuleClass {
    pub id: RuleClassId,
    pub attributes: Vec<AttributeSchema>,
    pub kind: RuleClassKind,
    pub native_permissive: bool,
}

impl RuleClass {
    pub fn attribute(&self, name: &str) -> Option<&AttributeSchema> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == name)
    }

    pub fn display_name(&self) -> &str {
        match &self.id {
            RuleClassId::Native(name) => name,
            RuleClassId::Starlark { exported_name, .. } => exported_name,
        }
    }
}

#[derive(Debug)]
pub(crate) enum RuleClassSpec<'a> {
    Native(&'static RuleClass),
    Starlark {
        id: RuleClassId,
        attributes: &'a [AttributeSchema],
        kind: RuleClassKind,
    },
}

#[derive(Debug, Clone)]
pub enum LoadedRuleClass {
    Native(&'static RuleClass),
    Starlark(OwnedFrozenValue),
}

impl LoadedRuleClass {
    pub fn attributes(&self) -> &[AttributeSchema] {
        match self {
            Self::Native(rule_class) => &rule_class.attributes,
            Self::Starlark(rule_class) => {
                crate::starlark::rule::frozen_rule_class_attributes(rule_class)
            }
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::Native(rule_class) => rule_class.display_name(),
            Self::Starlark(rule_class) => crate::starlark::rule::frozen_rule_class_name(rule_class),
        }
    }

    /// The name of a built-in rule class. Starlark rule classes return `None` even when exported
    /// under a built-in name, so built-in semantics are never applied to user-defined rules.
    pub fn native_name(&self) -> Option<&str> {
        match self {
            Self::Native(rule_class) => Some(rule_class.display_name()),
            Self::Starlark(_) => None,
        }
    }

    pub fn kind(&self) -> RuleClassKind {
        match self {
            Self::Native(rule_class) => rule_class.kind,
            Self::Starlark(rule_class) => crate::starlark::rule::frozen_rule_class_kind(rule_class),
        }
    }
}

impl RuleClassSpec<'_> {
    pub fn id(&self) -> &RuleClassId {
        match self {
            Self::Native(rule_class) => &rule_class.id,
            Self::Starlark { id, .. } => id,
        }
    }

    pub fn attributes(&self) -> &[AttributeSchema] {
        match self {
            Self::Native(rule_class) => &rule_class.attributes,
            Self::Starlark { attributes, .. } => attributes,
        }
    }

    pub fn native_permissive(&self) -> bool {
        matches!(self, Self::Native(rule_class) if rule_class.native_permissive)
    }
}

#[derive(Debug, Clone)]
pub struct RuleAttribute {
    pub value: AttributeValue,
    pub explicit: bool,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub rule_class: LoadedRuleClass,
    pub attributes: Box<[Option<RuleAttribute>]>,
}

impl Rule {
    pub fn rule_class(&self) -> &LoadedRuleClass {
        &self.rule_class
    }

    pub fn attribute(&self, name: &str) -> Option<&AttributeValue> {
        let index = self
            .rule_class
            .attributes()
            .iter()
            .position(|attribute| attribute.name == name)?;
        self.attributes[index]
            .as_ref()
            .map(|attribute| &attribute.value)
    }

    pub fn is_attribute_explicit(&self, name: &str) -> bool {
        let Some(index) = self
            .rule_class
            .attributes()
            .iter()
            .position(|attribute| attribute.name == name)
        else {
            return false;
        };
        self.attributes[index]
            .as_ref()
            .is_some_and(|attribute| attribute.explicit)
    }

    pub fn visit_dependency_labels(&self, mut visitor: impl FnMut(&CanonicalLabel<'static>)) {
        for (schema, attribute) in self
            .rule_class
            .attributes()
            .iter()
            .zip(self.attributes.iter())
        {
            if let Some(attribute) = attribute {
                visit_attribute_dependency_labels(schema, &attribute.value, &mut visitor);
            }
        }
    }

    pub fn output_labels(&self) -> Vec<&CanonicalLabel<'static>> {
        let mut labels = Vec::new();
        for (schema, attribute) in self
            .rule_class
            .attributes()
            .iter()
            .zip(self.attributes.iter())
        {
            if schema.dependency != AttributeDependency::Output {
                continue;
            }
            if let Some(attribute) = attribute {
                for part in &attribute.value.parts {
                    if let AttributeValuePart::Direct(value) = part {
                        value.visit_labels(|label| labels.push(label));
                    }
                }
            }
        }
        labels
    }
}

/// A select() condition that matches the configuration. Its requirements are kept to find the most
/// specific matching condition.
#[derive(Debug, Clone)]
pub struct ConditionMatch<'a> {
    pub requirements: ConfigurationRequirements<'a>,
}

impl AttributeValue {
    /// Resolves this value's select()s, calling `matches` to find the conditions that match.
    /// A value with a single part is borrowed; only concatenated parts are copied.
    pub fn resolve<'v, 'm>(
        &'v self,
        schema: &'v AttributeSchema,
        mut matches: impl FnMut(CanonicalLabel<'v>) -> anyhow::Result<Option<&'m ConditionMatch<'m>>>,
    ) -> anyhow::Result<Cow<'v, DirectAttributeValue>> {
        let mut resolve_part =
            |part: &'v AttributeValuePart| -> anyhow::Result<&'v DirectAttributeValue> {
                match part {
                    AttributeValuePart::Direct(value) => Ok(value),
                    AttributeValuePart::Selector(selector) => {
                        resolve_selector(schema, selector, &mut matches)
                    }
                }
            };
        let resolved = match self.parts.as_slice() {
            [part] => Cow::Borrowed(resolve_part(part)?),
            parts => {
                let values = parts
                    .iter()
                    .map(resolve_part)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Cow::Owned(concatenate_values(schema, &values)?)
            }
        };
        if self
            .parts
            .iter()
            .any(|part| matches!(part, AttributeValuePart::Selector(_)))
            && let DirectAttributeValue::LabelList(labels) = &*resolved
        {
            let mut unique = std::collections::BTreeSet::new();
            if labels.iter().any(|label| !unique.insert(label)) {
                anyhow::bail!(
                    "attribute `{}` contains a duplicate label after select() resolution",
                    schema.name
                );
            }
        }
        Ok(resolved)
    }
}

fn resolve_selector<'v, 'm>(
    schema: &'v AttributeSchema,
    selector: &'v Selector,
    matches: &mut impl FnMut(CanonicalLabel<'v>) -> anyhow::Result<Option<&'m ConditionMatch<'m>>>,
) -> anyhow::Result<&'v DirectAttributeValue> {
    let mut matching = Vec::new();
    for (label, branch) in &selector.entries {
        if let Some(condition) = matches(label.as_borrowed())? {
            matching.push((label, branch, condition));
        }
    }

    if matching.is_empty() {
        return match &selector.default {
            Some(branch) => branch_value(schema, branch),
            None if !selector.no_match_error.is_empty() => anyhow::bail!(
                "configurable attribute `{}` doesn't match this configuration: {}",
                schema.name,
                selector.no_match_error
            ),
            None => anyhow::bail!(
                "configurable attribute `{}` doesn't match this configuration (would a default condition help?)",
                schema.name
            ),
        };
    }

    // The most specific matching conditions: those that no other matching condition specializes.
    let maximal = matching
        .iter()
        .filter(|(_, _, candidate)| {
            !matching.iter().any(|(_, _, other)| {
                other
                    .requirements
                    .is_strict_specialization_of(&candidate.requirements)
            })
        })
        .map(|(label, branch, _)| Ok((*label, branch_value(schema, branch)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    // They must all select the same value. Specialization is a strict partial order, so at least
    // one condition is maximal.
    match maximal.split_first() {
        Some(((_, value), others)) if others.iter().all(|(_, other)| other == value) => Ok(value),
        _ => anyhow::bail!(
            "configurable attribute `{}` is ambiguous; matching conditions: {}",
            schema.name,
            comma_separated(maximal.iter().map(|(label, _)| label))
        ),
    }
}

/// Displays `items` separated by commas, writing each one straight to the output.
fn comma_separated<T: fmt::Display>(items: impl Iterator<Item = T> + Clone) -> impl fmt::Display {
    fmt::from_fn(move |formatter| {
        for (index, item) in items.clone().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            fmt::Display::fmt(&item, formatter)?;
        }
        Ok(())
    })
}

/// The value of a select() branch. As in Bazel, a `None` branch leaves the attribute as if it were
/// not set: it takes its default, and an optional label without one is unset.
fn branch_value<'v>(
    schema: &'v AttributeSchema,
    branch: &'v Option<DirectAttributeValue>,
) -> anyhow::Result<&'v DirectAttributeValue> {
    static UNSET: DirectAttributeValue = DirectAttributeValue::None;
    match branch {
        Some(value) => Ok(value),
        None if schema.mandatory => anyhow::bail!(
            "mandatory attribute `{}` resolved to None after evaluating select()",
            schema.name
        ),
        None => Ok(schema.default.as_ref().unwrap_or(&UNSET)),
    }
}

/// Concatenates the parts of a value, as `+` does when combining select()s.
fn concatenate_values<'v>(
    schema: &AttributeSchema,
    values: &[&'v DirectAttributeValue],
) -> anyhow::Result<DirectAttributeValue> {
    use DirectAttributeValue as Value;

    let incompatible =
        || anyhow::anyhow!("attribute `{}` has incompatible select values", schema.name);
    match schema.attribute_type {
        AttributeType::IntList => concatenate_lists(values, |value| match value {
            Value::IntList(list) => Ok(list),
            _ => Err(incompatible()),
        })
        .map(Value::IntList),
        AttributeType::StringList => concatenate_lists(values, |value| match value {
            Value::StringList(list) => Ok(list),
            _ => Err(incompatible()),
        })
        .map(Value::StringList),
        AttributeType::LabelList => concatenate_lists(values, |value| match value {
            Value::LabelList(list) => Ok(list),
            _ => Err(incompatible()),
        })
        .map(Value::LabelList),
        AttributeType::String => {
            let string = |value: &'v DirectAttributeValue| -> anyhow::Result<&'v str> {
                match value {
                    Value::String(string) => Ok(string),
                    _ => Err(incompatible()),
                }
            };
            let length = values
                .iter()
                .map(|value| string(value).map(str::len))
                .sum::<anyhow::Result<usize>>()?;
            let mut combined = String::with_capacity(length);
            for value in values {
                combined.push_str(string(value)?);
            }
            Ok(Value::String(combined))
        }
        _ => anyhow::bail!(
            "attribute `{}` cannot concatenate select values",
            schema.name
        ),
    }
}

/// Concatenates the lists that `list` extracts from `values`, allocating the result once.
fn concatenate_lists<'v, T: Clone + 'v>(
    values: &[&'v DirectAttributeValue],
    list: impl Fn(&'v DirectAttributeValue) -> anyhow::Result<&'v Vec<T>>,
) -> anyhow::Result<Vec<T>> {
    let length = values
        .iter()
        .map(|value| list(value).map(Vec::len))
        .sum::<anyhow::Result<usize>>()?;
    let mut combined = Vec::with_capacity(length);
    for value in values {
        combined.extend_from_slice(list(value)?);
    }
    Ok(combined)
}

pub(crate) fn visit_attribute_dependency_labels(
    schema: &AttributeSchema,
    value: &AttributeValue,
    mut visitor: impl FnMut(&CanonicalLabel<'static>),
) {
    let visit_value = |value: &DirectAttributeValue,
                       visitor: &mut dyn FnMut(&CanonicalLabel<'static>)| {
        if schema.dependency == AttributeDependency::Dependency {
            value.visit_labels(visitor);
        }
    };

    for part in &value.parts {
        match part {
            AttributeValuePart::Direct(value) => visit_value(value, &mut visitor),
            AttributeValuePart::Selector(selector) => {
                for (condition, value) in &selector.entries {
                    if schema.selector_conditions_are_dependencies {
                        visitor(condition);
                    }
                    match value {
                        Some(value) => visit_value(value, &mut visitor),
                        None => {
                            if let Some(default) = &schema.default {
                                visit_value(default, &mut visitor);
                            }
                        }
                    }
                }
                if let Some(branch) = &selector.default {
                    match branch {
                        Some(value) => visit_value(value, &mut visitor),
                        None => {
                            if let Some(default) = &schema.default {
                                visit_value(default, &mut visitor);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn base_attribute(
    name: &str,
    attribute_type: AttributeType,
    default: DirectAttributeValue,
    dependency: AttributeDependency,
) -> AttributeSchema {
    AttributeSchema::new(name, attribute_type, Some(default), dependency)
}

pub fn base_rule_attributes(kind: RuleClassKind) -> Vec<AttributeSchema> {
    let mut attributes = vec![
        {
            let mut name = AttributeSchema::new(
                "name",
                AttributeType::String,
                None,
                AttributeDependency::NoDependency,
            );
            name.mandatory = true;
            name.configurable = false;
            name.allow_empty = Some(false);
            name
        },
        {
            let mut visibility = base_attribute(
                "visibility",
                AttributeType::LabelList,
                DirectAttributeValue::LabelList(Vec::new()),
                AttributeDependency::NoDependency,
            );
            visibility.selector_conditions_are_dependencies = false;
            visibility
        },
        base_attribute(
            "transitive_configs",
            AttributeType::LabelList,
            DirectAttributeValue::LabelList(Vec::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "deprecation",
            AttributeType::String,
            DirectAttributeValue::String(String::new()),
            AttributeDependency::NoDependency,
        ),
        base_attribute(
            "tags",
            AttributeType::StringList,
            DirectAttributeValue::StringList(Vec::new()),
            AttributeDependency::NoDependency,
        ),
        base_attribute(
            "testonly",
            AttributeType::Bool,
            DirectAttributeValue::Bool(false),
            AttributeDependency::NoDependency,
        ),
        base_attribute(
            "features",
            AttributeType::StringList,
            DirectAttributeValue::StringList(Vec::new()),
            AttributeDependency::NoDependency,
        ),
        base_attribute(
            "compatible_with",
            AttributeType::LabelList,
            DirectAttributeValue::LabelList(Vec::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "restricted_to",
            AttributeType::LabelList,
            DirectAttributeValue::LabelList(Vec::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "package_metadata",
            AttributeType::LabelList,
            DirectAttributeValue::LabelList(Vec::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "aspect_hints",
            AttributeType::LabelList,
            DirectAttributeValue::LabelList(Vec::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "toolchains",
            AttributeType::LabelList,
            DirectAttributeValue::LabelList(Vec::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "exec_properties",
            AttributeType::StringDict,
            DirectAttributeValue::StringDict(BTreeMap::new()),
            AttributeDependency::NoDependency,
        ),
        base_attribute(
            "exec_compatible_with",
            AttributeType::LabelList,
            DirectAttributeValue::LabelList(Vec::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "exec_group_compatible_with",
            AttributeType::LabelListDict,
            DirectAttributeValue::LabelListDict(BTreeMap::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "target_compatible_with",
            AttributeType::LabelList,
            DirectAttributeValue::LabelList(Vec::new()),
            AttributeDependency::Dependency,
        ),
        base_attribute(
            "expect_failure",
            AttributeType::String,
            DirectAttributeValue::String(String::new()),
            AttributeDependency::NoDependency,
        ),
    ];

    if matches!(kind, RuleClassKind::Executable | RuleClassKind::Test) {
        attributes.extend([
            base_attribute(
                "args",
                AttributeType::StringList,
                DirectAttributeValue::StringList(Vec::new()),
                AttributeDependency::NoDependency,
            ),
            base_attribute(
                "env",
                AttributeType::StringDict,
                DirectAttributeValue::StringDict(BTreeMap::new()),
                AttributeDependency::NoDependency,
            ),
        ]);
    }

    if kind == RuleClassKind::Test {
        attributes.extend([
            base_attribute(
                "size",
                AttributeType::String,
                DirectAttributeValue::String("medium".to_owned()),
                AttributeDependency::NoDependency,
            ),
            base_attribute(
                "timeout",
                AttributeType::String,
                DirectAttributeValue::String("moderate".to_owned()),
                AttributeDependency::NoDependency,
            ),
            base_attribute(
                "flaky",
                AttributeType::Bool,
                DirectAttributeValue::Bool(false),
                AttributeDependency::NoDependency,
            ),
            base_attribute(
                "shard_count",
                AttributeType::Int,
                DirectAttributeValue::Int(-1),
                AttributeDependency::NoDependency,
            ),
            base_attribute(
                "local",
                AttributeType::Bool,
                DirectAttributeValue::Bool(false),
                AttributeDependency::NoDependency,
            ),
            base_attribute(
                "env_inherit",
                AttributeType::StringList,
                DirectAttributeValue::StringList(Vec::new()),
                AttributeDependency::NoDependency,
            ),
        ]);
    }

    attributes
}

pub fn native_rule_class(name: &str, attributes: Vec<AttributeSchema>) -> RuleClass {
    native_rule_class_with_kind(name, RuleClassKind::Ordinary, attributes)
}

pub fn native_rule_class_with_kind(
    name: &str,
    kind: RuleClassKind,
    mut attributes: Vec<AttributeSchema>,
) -> RuleClass {
    let mut all_attributes = base_rule_attributes(kind);
    all_attributes.append(&mut attributes);
    RuleClass {
        id: RuleClassId::Native(name.to_owned()),
        attributes: all_attributes,
        kind,
        native_permissive: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bazel::label::MAIN_REPO;

    fn label(name: &str) -> CanonicalLabel<'static> {
        CanonicalLabel::new(MAIN_REPO, "", name.to_owned())
    }

    fn labels(names: &[&str]) -> DirectAttributeValue {
        DirectAttributeValue::LabelList(names.iter().map(|name| label(name)).collect())
    }

    fn string(value: &str) -> DirectAttributeValue {
        DirectAttributeValue::String(value.to_owned())
    }

    fn selector(
        entries: impl IntoIterator<Item = (&'static str, DirectAttributeValue)>,
    ) -> AttributeValuePart {
        AttributeValuePart::Selector(Selector {
            entries: entries
                .into_iter()
                .map(|(condition, value)| (label(condition), Some(value)))
                .collect(),
            default: None,
            no_match_error: String::new(),
        })
    }

    fn requiring<'a>(values: impl IntoIterator<Item = (&'a str, &'a str)>) -> ConditionMatch<'a> {
        ConditionMatch {
            requirements: ConfigurationRequirements {
                values: values.into_iter().collect(),
                ..ConfigurationRequirements::default()
            },
        }
    }

    #[test]
    fn single_parts_are_borrowed_and_concatenations_are_built_once() {
        let srcs = AttributeSchema::new(
            "srcs",
            AttributeType::LabelList,
            Some(labels(&[])),
            AttributeDependency::Dependency,
        );
        let arm = requiring([("cpu", "arm")]);
        let matches =
            |condition: CanonicalLabel<'_>| Ok((condition.name() == "arm").then_some(&arm));

        let direct = AttributeValue::direct(labels(&["a.txt"]));
        let resolved = direct.resolve(&srcs, matches).unwrap();
        assert!(matches!(resolved, Cow::Borrowed(value) if *value == labels(&["a.txt"])));

        let selected = AttributeValue {
            parts: vec![selector([
                ("arm", labels(&["arm.txt"])),
                ("x86", labels(&["x86.txt"])),
            ])],
        };
        let resolved = selected.resolve(&srcs, matches).unwrap();
        assert!(matches!(resolved, Cow::Borrowed(value) if *value == labels(&["arm.txt"])));

        let concatenated = AttributeValue {
            parts: vec![
                AttributeValuePart::Direct(labels(&["a.txt", "b.txt"])),
                selector([("arm", labels(&["arm.txt"]))]),
            ],
        };
        let resolved = concatenated.resolve(&srcs, matches).unwrap();
        assert!(matches!(resolved, Cow::Owned(_)));
        assert_eq!(*resolved, labels(&["a.txt", "b.txt", "arm.txt"]));

        let message = AttributeSchema::new(
            "message",
            AttributeType::String,
            Some(string("")),
            AttributeDependency::NoDependency,
        );
        let selected = AttributeValue {
            parts: vec![
                AttributeValuePart::Direct(string("prefix ")),
                selector([("arm", string("arm"))]),
            ],
        };
        assert_eq!(
            *selected.resolve(&message, matches).unwrap(),
            string("prefix arm")
        );
        let mixed = AttributeValue {
            parts: vec![
                AttributeValuePart::Direct(string("prefix ")),
                AttributeValuePart::Direct(labels(&["a.txt"])),
            ],
        };
        assert_eq!(
            mixed.resolve(&message, matches).unwrap_err().to_string(),
            "attribute `message` has incompatible select values"
        );
    }

    #[test]
    fn the_most_specific_conditions_must_agree() {
        let srcs = AttributeSchema::new(
            "srcs",
            AttributeType::LabelList,
            Some(labels(&[])),
            AttributeDependency::Dependency,
        );
        let arm = requiring([("cpu", "arm")]);
        let arm_dbg = requiring([("compilation_mode", "dbg"), ("cpu", "arm")]);
        let conditions = BTreeMap::from([
            ("arm", &arm),
            ("arm_dbg", &arm_dbg),
            ("other_arm_dbg", &arm_dbg),
        ]);
        let matches = |condition: CanonicalLabel<'_>| Ok(conditions.get(condition.name()).copied());
        let value = |arm_dbg: &str, other_arm_dbg: &str| AttributeValue {
            parts: vec![selector([
                ("arm", labels(&["arm.txt"])),
                ("arm_dbg", labels(&[arm_dbg])),
                ("other_arm_dbg", labels(&[other_arm_dbg])),
            ])],
        };

        // Both specializations of `arm` match. They may select the same value...
        let agreeing = value("dbg.txt", "dbg.txt");
        assert_eq!(
            *agreeing.resolve(&srcs, matches).unwrap(),
            labels(&["dbg.txt"])
        );
        // ...but not different ones. The error lists them without the less specific `arm`.
        let conflicting = value("dbg.txt", "other.txt");
        assert_eq!(
            conflicting.resolve(&srcs, matches).unwrap_err().to_string(),
            format!(
                "configurable attribute `srcs` is ambiguous; matching conditions: {}, {}",
                label("arm_dbg"),
                label("other_arm_dbg")
            )
        );
    }

    #[test]
    fn none_branches_take_the_default_or_leave_labels_unset() {
        let none_by_default = AttributeValue {
            parts: vec![AttributeValuePart::Selector(Selector {
                entries: BTreeMap::new(),
                default: Some(None),
                no_match_error: String::new(),
            })],
        };
        let matches = |_: CanonicalLabel<'_>| Ok(None);
        let optional = AttributeSchema::new(
            "dep",
            AttributeType::Label,
            None,
            AttributeDependency::Dependency,
        );
        assert_eq!(
            *none_by_default.resolve(&optional, matches).unwrap(),
            DirectAttributeValue::None
        );
        let srcs = AttributeSchema::new(
            "srcs",
            AttributeType::LabelList,
            Some(labels(&["default.txt"])),
            AttributeDependency::Dependency,
        );
        assert_eq!(
            *none_by_default.resolve(&srcs, matches).unwrap(),
            labels(&["default.txt"])
        );
        let mut required = optional.clone();
        required.mandatory = true;
        assert_eq!(
            none_by_default
                .resolve(&required, matches)
                .unwrap_err()
                .to_string(),
            "mandatory attribute `dep` resolved to None after evaluating select()"
        );
    }

    #[test]
    fn comma_separated_writes_items_in_order() {
        assert_eq!(
            comma_separated(["a", "b", "c"].iter()).to_string(),
            "a, b, c"
        );
        assert_eq!(comma_separated(["a"].iter()).to_string(), "a");
        assert_eq!(comma_separated(std::iter::empty::<&str>()).to_string(), "");
    }
}
