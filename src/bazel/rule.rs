#![allow(dead_code)]

use crate::bazel::config::ConfigurationRequirements;
use crate::bazel::label::CanonicalLabel;
use starlark::values::OwnedFrozenValue;
use std::collections::BTreeMap;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AttributeTransition {
    #[default]
    None,
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
            transition: AttributeTransition::None,
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

#[derive(Debug, Clone)]
pub struct ConditionMatch {
    pub requirements: ConfigurationRequirements,
}

impl AttributeValue {
    pub fn resolve(
        &self,
        schema: &AttributeSchema,
        mut matches: impl FnMut(&CanonicalLabel<'static>) -> anyhow::Result<Option<ConditionMatch>>,
    ) -> anyhow::Result<DirectAttributeValue> {
        let mut resolved = Vec::with_capacity(self.parts.len());
        for part in &self.parts {
            let value = match part {
                AttributeValuePart::Direct(value) => value.clone(),
                AttributeValuePart::Selector(selector) => {
                    resolve_selector(schema, selector, &mut matches)?
                }
            };
            resolved.push(value);
        }
        let resolved = concatenate_values(schema, resolved)?;
        if self
            .parts
            .iter()
            .any(|part| matches!(part, AttributeValuePart::Selector(_)))
            && let DirectAttributeValue::LabelList(labels) = &resolved
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

fn resolve_selector(
    schema: &AttributeSchema,
    selector: &Selector,
    matches: &mut impl FnMut(&CanonicalLabel<'static>) -> anyhow::Result<Option<ConditionMatch>>,
) -> anyhow::Result<DirectAttributeValue> {
    let mut matching = Vec::new();
    for (label, branch) in &selector.entries {
        if let Some(condition) = matches(label)? {
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

    let maximal = matching.iter().filter(|(_, _, candidate)| {
        !matching.iter().any(|(_, _, other)| {
            other
                .requirements
                .is_strict_specialization_of(&candidate.requirements)
        })
    });
    let maximal = maximal
        .map(|(label, branch, _)| Ok((*label, branch_value(schema, branch)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let Some((_, value)) = maximal.first() else {
        let labels = matching
            .iter()
            .map(|(label, _, _)| label.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "configurable attribute `{}` is ambiguous; matching conditions: {labels}",
            schema.name
        );
    };
    if maximal.iter().all(|(_, candidate)| candidate == value) {
        return Ok(value.clone());
    }
    let labels = maximal
        .iter()
        .map(|(label, _)| label.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "configurable attribute `{}` is ambiguous; matching conditions: {labels}",
        schema.name
    )
}

fn branch_value(
    schema: &AttributeSchema,
    branch: &Option<DirectAttributeValue>,
) -> anyhow::Result<DirectAttributeValue> {
    branch
        .as_ref()
        .or(schema.default.as_ref())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("attribute `{}` has no default value", schema.name))
}

fn concatenate_values(
    schema: &AttributeSchema,
    mut values: Vec<DirectAttributeValue>,
) -> anyhow::Result<DirectAttributeValue> {
    if values.len() == 1 {
        return Ok(values.pop().unwrap());
    }
    match schema.attribute_type {
        AttributeType::IntList => {
            let mut combined = Vec::new();
            for value in values {
                let DirectAttributeValue::IntList(mut value) = value else {
                    anyhow::bail!("attribute `{}` has incompatible select values", schema.name);
                };
                combined.append(&mut value);
            }
            Ok(DirectAttributeValue::IntList(combined))
        }
        AttributeType::String => {
            let mut combined = String::new();
            for value in values {
                let DirectAttributeValue::String(value) = value else {
                    anyhow::bail!("attribute `{}` has incompatible select values", schema.name);
                };
                combined.push_str(&value);
            }
            Ok(DirectAttributeValue::String(combined))
        }
        AttributeType::StringList => {
            let mut combined = Vec::new();
            for value in values {
                let DirectAttributeValue::StringList(mut value) = value else {
                    anyhow::bail!("attribute `{}` has incompatible select values", schema.name);
                };
                combined.append(&mut value);
            }
            Ok(DirectAttributeValue::StringList(combined))
        }
        AttributeType::LabelList => {
            let mut combined = Vec::new();
            for value in values {
                let DirectAttributeValue::LabelList(mut value) = value else {
                    anyhow::bail!("attribute `{}` has incompatible select values", schema.name);
                };
                combined.append(&mut value);
            }
            Ok(DirectAttributeValue::LabelList(combined))
        }
        _ => anyhow::bail!(
            "attribute `{}` cannot concatenate select values",
            schema.name
        ),
    }
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
