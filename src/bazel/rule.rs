#![allow(dead_code)]

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
    String,
    StringList,
    Label,
    LabelList,
    Output,
    OutputList,
    StringDict,
    LabelListDict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeDependency {
    Dependency,
    NoDependency,
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectAttributeValue {
    Bool(bool),
    Int(i64),
    String(String),
    StringList(Vec<String>),
    Label(CanonicalLabel<'static>),
    LabelList(Vec<CanonicalLabel<'static>>),
    Output(CanonicalLabel<'static>),
    OutputList(Vec<CanonicalLabel<'static>>),
    StringDict(BTreeMap<String, String>),
    LabelListDict(BTreeMap<String, Vec<CanonicalLabel<'static>>>),
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
            Self::Bool(_)
            | Self::Int(_)
            | Self::String(_)
            | Self::StringList(_)
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

pub fn native_rule_class(name: &str, mut attributes: Vec<AttributeSchema>) -> RuleClass {
    let mut all_attributes = base_rule_attributes(RuleClassKind::Ordinary);
    all_attributes.append(&mut attributes);
    RuleClass {
        id: RuleClassId::Native(name.to_owned()),
        attributes: all_attributes,
        kind: RuleClassKind::Ordinary,
        native_permissive: true,
    }
}
