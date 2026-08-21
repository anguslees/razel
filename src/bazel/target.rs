#![allow(dead_code)]

use crate::bazel::label::CanonicalLabel;
use crate::bazel::rule::Rule;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Rule,
    SourceFile,
    GeneratedFile,
}

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub visibility: Vec<CanonicalLabel<'static>>,
}

#[derive(Debug, Clone)]
pub struct GeneratedFile {
    pub generating_rule: String,
}

#[derive(Debug, Clone)]
pub enum Target {
    Rule(Rule),
    SourceFile(SourceFile),
    GeneratedFile(GeneratedFile),
}

impl Target {
    pub fn target_kind(&self) -> TargetKind {
        match self {
            Self::Rule(_) => TargetKind::Rule,
            Self::SourceFile(_) => TargetKind::SourceFile,
            Self::GeneratedFile(_) => TargetKind::GeneratedFile,
        }
    }

    pub fn as_rule(&self) -> Option<&Rule> {
        match self {
            Self::Rule(rule) => Some(rule),
            Self::SourceFile(_) | Self::GeneratedFile(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TargetView<'a> {
    label: CanonicalLabel<'a>,
    target: &'a Target,
}

impl<'a> TargetView<'a> {
    pub(crate) fn new(label: CanonicalLabel<'a>, target: &'a Target) -> Self {
        Self { label, target }
    }

    pub fn name(&self) -> &str {
        self.label.name()
    }

    pub fn owned_name(&self) -> String {
        self.label.name().to_owned()
    }

    pub fn target(&self) -> &'a Target {
        self.target
    }

    pub fn label(&self) -> &CanonicalLabel<'a> {
        &self.label
    }

    pub fn target_kind(&self) -> TargetKind {
        self.target.target_kind()
    }

    pub fn as_rule(&self) -> Option<&'a Rule> {
        self.target.as_rule()
    }

    pub fn generating_rule(&self) -> Option<CanonicalLabel<'_>> {
        match self.target {
            Target::GeneratedFile(generated) => Some(CanonicalLabel::new(
                self.label.repo.as_borrowed(),
                self.label.package(),
                generated.generating_rule.as_str(),
            )),
            Target::Rule(_) | Target::SourceFile(_) => None,
        }
    }

    pub fn repository_path(&self) -> Option<Cow<'_, str>> {
        if !matches!(self.target, Target::SourceFile(_)) {
            return None;
        }
        if self.label.package().is_empty() {
            Some(Cow::Borrowed(self.label.name()))
        } else {
            Some(Cow::Owned(format!(
                "{}/{}",
                self.label.package(),
                self.label.name()
            )))
        }
    }
}
