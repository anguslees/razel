// src/bazel/label.rs

#![allow(dead_code, unused)]

use chumsky::prelude::*;
use std::{borrow::Cow, fmt, ops::Deref};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApparentRepo<'a>(Cow<'a, str>);

impl<'a> ApparentRepo<'a> {
    pub fn new(name: impl Into<Cow<'a, str>>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl<'a> ApparentRepo<'a> {
    pub fn into_name(self) -> Cow<'a, str> {
        self.0
    }
}

impl<'a> AsRef<str> for ApparentRepo<'a> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'a> fmt::Display for ApparentRepo<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}

/// An empty string means the main repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalRepo<'a>(Cow<'a, str>);

impl<'a> CanonicalRepo<'a> {
    pub fn new(name: impl Into<Cow<'a, str>>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl<'a> CanonicalRepo<'a> {
    pub fn into_name(self) -> Cow<'a, str> {
        self.0
    }
}

impl<'a> AsRef<str> for CanonicalRepo<'a> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'a> fmt::Display for CanonicalRepo<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@@{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Repo<'a> {
    Apparent(ApparentRepo<'a>),
    Canonical(CanonicalRepo<'a>),
}

impl<'a> Repo<'a> {
    pub fn into_name(self) -> Cow<'a, str> {
        match self {
            Repo::Apparent(r) => r.into_name(),
            Repo::Canonical(r) => r.into_name(),
        }
    }
}

impl<'a> fmt::Display for Repo<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Repo::Apparent(r) => r.fmt(f),
            Repo::Canonical(r) => r.fmt(f),
        }
    }
}

impl<'a> AsRef<str> for Repo<'a> {
    fn as_ref(&self) -> &str {
        match self {
            Repo::Apparent(r) => r.as_ref(),
            Repo::Canonical(r) => r.as_ref(),
        }
    }
}

impl<'a> From<CanonicalRepo<'a>> for Repo<'a> {
    fn from(r: CanonicalRepo<'a>) -> Self {
        Repo::Canonical(r)
    }
}

impl<'a> From<&CanonicalRepo<'a>> for Repo<'a> {
    fn from(r: &CanonicalRepo<'a>) -> Self {
        Repo::Canonical(r.clone())
    }
}

impl<'a> From<ApparentRepo<'a>> for Repo<'a> {
    fn from(r: ApparentRepo<'a>) -> Self {
        Repo::Apparent(r)
    }
}

impl<'a> From<&ApparentRepo<'a>> for Repo<'a> {
    fn from(r: &ApparentRepo<'a>) -> Self {
        Repo::Apparent(r.clone())
    }
}

impl<'a> From<&Repo<'a>> for Repo<'a> {
    fn from(r: &Repo<'a>) -> Self {
        r.clone()
    }
}

/// A Bazel label, identifying a repo target.
///
/// A label has the form:
///
/// `[@|@@][repo_name]//[package_path]:[target_name]`
///
/// See https://bazel.build/concepts/labels
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Label<'a, R = Repo<'a>> {
    /// The repository name, e.g., `my_repo`.
    pub repo: R,
    /// The package path, e.g., `my/package`.
    pub package: Cow<'a, str>,
    /// The target name, e.g., `my_target`.
    pub target: Cow<'a, str>,
}

/// A Bazel label, identifying a canonical repo target.
///
/// A canonical-repo label has the form:
///
/// `@@[repo_name]//[package_path]:[target_name]`
///
/// See https://bazel.build/concepts/labels
pub type CanonicalLabel<'a> = Label<'a, CanonicalRepo<'a>>;

/// A Bazel label, identifying an apparent repo target.
///
/// An apparent-repo label has the form:
///
/// `@[repo_name]//[package_path]:[target_name]`
///
/// See https://bazel.build/concepts/labels
pub type ApparentLabel<'a> = Label<'a, ApparentRepo<'a>>;

/// The well-known 'main' repo representing the main workspace.
/// ```
/// use crate::bazel::label::MAIN_REPO;
///
/// assert_eq!(MAIN_REPO.to_string(), "@@");
/// ```
pub const MAIN_REPO: CanonicalRepo<'static> = CanonicalRepo(Cow::Borrowed(""));

/// A label representing the well-known 'main' repo root.  This is the workspace root.
/// ```
/// use crate::bazel::label::MAIN_REPO_ROOT;
///
/// assert_eq!(MAIN_REPO_ROOT.to_string(), "@@//");
/// ```
pub const MAIN_REPO_ROOT: CanonicalLabel<'static> = CanonicalLabel {
    repo: MAIN_REPO,
    package: Cow::Borrowed(""),
    target: Cow::Borrowed(""),
};

impl<'a, R> Label<'a, R> {
    /// Creates a new `Label`.
    pub fn new(repo: R, package: impl Into<Cow<'a, str>>, target: impl Into<Cow<'a, str>>) -> Self {
        Self {
            repo,
            package: package.into(),
            target: target.into(),
        }
    }
}

impl<'a> ApparentLabel<'a> {
    /// Converts this apparent label to a canonical label, given a repo mapping.
    ///
    /// ```
    /// use crate::bazel::label::{ApparentLabel, ApparentRepo, CanonicalRepo};
    /// use std::borrow::Cow;
    /// use std::collections::HashMap;
    ///
    /// let mut repo_mapping = HashMap::new();
    /// repo_mapping.insert("my_repo", "my_repo_canon");
    ///
    /// let apparent_label = ApparentLabel::new(ApparentRepo::new("my_repo"), "my/package", "my_target");
    ///
    /// let canonical_label = apparent_label.into_canonical(|l| repo_mapping.get(l.as_str()).map(|&s| CanonicalRepo::new(s))).unwrap();
    /// assert_eq!(canonical_label.to_string(), "@@my_repo_canon//my/package:my_target");
    /// ```
    pub fn into_canonical<F>(self, func: F) -> Option<CanonicalLabel<'a>>
    where
        F: FnOnce(&ApparentRepo<'a>) -> Option<CanonicalRepo<'a>>,
    {
        func(&self.repo).map(|canonical_name| CanonicalLabel {
            repo: canonical_name,
            package: self.package,
            target: self.target,
        })
    }
}

impl<'a> Label<'a, Repo<'a>> {
    /// Converts this apparent label to a canonical label, given a repo mapping.
    ///
    /// ```
    /// use crate::bazel::label::{parse_label, Label, Repo, CanonicalRepo, MAIN_REPO_ROOT};
    /// use std::collections::HashMap;
    ///
    /// let mut repo_mapping = HashMap::new();
    /// repo_mapping.insert("my_repo", "my_repo_canon");
    ///
    /// let label = parse_label("@my_repo//my/package:my_target", &MAIN_REPO_ROOT).unwrap();
    ///
    /// let canonical_label = label.into_canonical(|l| repo_mapping.get(l.as_str()).map(|s| CanonicalRepo::new(*s))).unwrap();
    /// assert_eq!(canonical_label.to_string(), "@@my_repo_canon//my/package:my_target");
    ///
    /// let label2 = parse_label("@@canon_repo//my/package:my_target", &MAIN_REPO_ROOT).unwrap();
    ///
    /// let canonical_label2 = label2.into_canonical(|l| repo_mapping.get(l.as_str()).map(|s| CanonicalRepo::new(*s))).unwrap();
    /// assert_eq!(canonical_label2.to_string(), "@@canon_repo//my/package:my_target");
    /// ```
    pub fn into_canonical<F>(self, func: F) -> Option<CanonicalLabel<'a>>
    where
        F: FnOnce(&ApparentRepo<'a>) -> Option<CanonicalRepo<'a>>,
    {
        match self.repo {
            Repo::Apparent(r) => func(&r),
            Repo::Canonical(r) => Some(r),
        }
        .map(|repo| CanonicalLabel {
            repo,
            package: self.package,
            target: self.target,
        })
    }
}

impl<'a, R: fmt::Display> fmt::Debug for Label<'a, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Label(\"{}//{}:{}\")",
            self.repo, self.package, self.target
        )
    }
}

impl<'a, R: fmt::Display + AsRef<str>> fmt::Display for Label<'a, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.package.is_empty() && self.target == self.repo_name() {
            return write!(f, "{}//", self.repo);
        }

        let pkg_str = &self.package;
        let last_pkg_segment = pkg_str
            .rsplit_once('/')
            .map(|(_, last)| last)
            .unwrap_or(pkg_str);

        if !pkg_str.is_empty() && last_pkg_segment == self.target {
            return write!(f, "{}//{}", self.repo, self.package);
        }

        write!(f, "{}//{}:{}", self.repo, self.package, self.target)
    }
}

impl<'a, R> Label<'a, R> {
    /// The name of the target. Corresponds to `Label.name` in Starlark.
    pub fn name(&self) -> &str {
        self.target.as_ref()
    }

    /// The name of the package. Corresponds to `Label.package` in Starlark.
    pub fn package(&self) -> &str {
        self.package.as_ref()
    }

    /// The repository part of the label. Corresponds to `Label.repo` in Starlark.
    pub fn repo(&self) -> &R {
        &self.repo
    }
}

impl<'a, R> Label<'a, R> {
    /// Creates a new label in the same package as this label.
    /// Corresponds to `Label.same_package_label()` in Starlark.
    pub fn same_package_label(&self, name: impl Into<Cow<'a, str>>) -> Label<'a, R>
    where
        R: Clone,
    {
        Label {
            repo: self.repo.clone(),
            package: self.package.clone(),
            target: name.into(),
        }
    }

    /// Resolves a relative label string.
    /// Corresponds to `Label.relative()` in Starlark.
    pub fn relative<'b>(&'b self, rel_path: &'b str) -> Result<Label<'b, Repo<'b>>, ParseError<'b>>
    where
        for<'c> &'c R: Into<Repo<'b>>,
    {
        parse_label(rel_path, self)
    }
}

impl<'a, R> Label<'a, R> {
    /// The name of the repository in which this target is defined.
    /// An alias for `repo()`.
    pub fn repo_name(&self) -> &str
    where
        R: AsRef<str>,
    {
        self.repo.as_ref()
    }

    /// The execution-time path of the workspace in which this target is defined.
    /// Corresponds to `Label.workspace_root` in Starlark.
    pub fn workspace_root(&self) -> String
    where
        R: AsRef<str>,
    {
        if self.repo.as_ref().is_empty() {
            "".to_string()
        } else {
            format!("external/{}", self.repo_name())
        }
    }
}

/// The pieces of a parsed label, used in intermediate calculations.
#[derive(PartialEq, Debug)]
struct RelativeLabel<'a> {
    repo: Option<Repo<'a>>,
    package: Option<Cow<'a, str>>,
    target: Option<Cow<'a, str>>,
}

type ParseError<'a> = Rich<'a, char>;

fn alphanumeric<'a>() -> impl chumsky::Parser<'a, &'a str, char, extra::Err<ParseError<'a>>> + Clone
{
    any()
        .filter(|c: &char| c.is_ascii_alphanumeric())
        .labelled("alphanumeric")
}

fn target_name_parser<'a>()
-> impl chumsky::Parser<'a, &'a str, &'a str, extra::Err<ParseError<'a>>> + Clone {
    choice((one_of(r##"!%@^_"#$&'()*-+,;<=>?[]{|}~."##), alphanumeric()))
        .labelled("valid target character")
        .repeated()
        .at_least(1)
        .to_slice()
        .validate(|x, e, emitter| {
            if x == "." || x == ".." {
                emitter.emit(Rich::custom(e.span(), "Target can't include . or .."))
            }
            x
        })
        .separated_by(just('/'))
        .at_least(1)
        .to_slice()
        .labelled("target name")
}

fn package_name_parser<'a>()
-> impl chumsky::Parser<'a, &'a str, &'a str, extra::Err<ParseError<'a>>> + Clone {
    choice((one_of(r##"! "#$%&'()*+,-.;<=>?@[]^_`{|}"##), alphanumeric()))
        .labelled("valid package character")
        .repeated()
        .at_least(1)
        .to_slice()
        .validate(|x: &str, e, emitter| {
            if x.chars().all(|c: char| c == '.') {
                emitter.emit(Rich::custom(
                    e.span(),
                    "Package can't include an all-dots segment",
                ))
            }
            x
        })
        .separated_by(just('/'))
        .to_slice()
        .labelled("package name")
}

fn repo_parser<'a>()
-> impl chumsky::Parser<'a, &'a str, Repo<'a>, extra::Err<ParseError<'a>>> + Clone {
    let repo_name = choice((one_of(r##"+_.-"##), alphanumeric()))
        .labelled("valid repo character")
        .repeated()
        .to_slice()
        .labelled("repository name");

    choice((
        just("@@")
            .ignore_then(repo_name.clone())
            .map(|s: &str| Repo::Canonical(CanonicalRepo::new(s)))
            .labelled("canonical repo"),
        just('@')
            .ignore_then(repo_name)
            .map(|s: &str| Repo::Apparent(ApparentRepo::new(s)))
            .labelled("apparent repo"),
    ))
}

fn parser<'a>() -> impl chumsky::Parser<'a, &'a str, RelativeLabel<'a>, extra::Err<ParseError<'a>>>
{
    // See https://bazel.build/concepts/labels#labels-lexical-specification

    let target = target_name_parser();
    let package = package_name_parser();
    let repo = repo_parser();

    (repo.or_not())
        .then(just("//").ignore_then(package).map(Some::<&str>))
        .then((just(':').ignore_then(target.clone())).or_not())
        .map(|((r, p), t)| match ((r, p), t) {
            // Expand shorthand: @repo// -> @repo//:repo
            ((Some(repo), Some(pkg)), None) if pkg.is_empty() => {
                let name = repo.clone().into_name();
                ((Some(repo), Some(pkg)), Some(name))
            }
            // Expand shorthand: @repo//my/pkg -> @repo//my/pkg:pkg
            ((r, Some(pkg)), None) => {
                let tgt = pkg.rsplit_once('/').map(|(_, tgt)| tgt).unwrap_or(pkg);
                ((r, Some(pkg)), Some(Cow::Borrowed(tgt)))
            }
            ((r, p), Some(t)) => ((r, p), Some(Cow::Borrowed(t))),
            ((r, p), None) => ((r, p), None),
        })
        .validate(|((r, p), t), e, emitter| {
            if r.is_none() && p.is_none() && t.is_none() {
                emitter.emit(Rich::custom(e.span(), "invalid label"));
            }
            ((r, p), t)
        })
        .or(just(':')
            .or_not()
            .ignore_then(target)
            .map(|t| ((None, None), Some(Cow::Borrowed(t)))))
        .labelled("label")
        .map(|((repo, package), target)| RelativeLabel {
            repo,
            package: package.map(Cow::Borrowed),
            target,
        })
}

/// Parses a label string.
pub fn parse_label<'a, R>(
    s: &'a str,
    context: &Label<'a, R>,
) -> Result<Label<'a, Repo<'a>>, ParseError<'a>>
where
    for<'b> &'b R: Into<Repo<'a>>,
{
    //log::debug!("Label parser grammar is {}", parser().debug().to_ebnf());

    let relative = parser()
        .parse(s)
        .into_result()
        .map_err(|errs| errs.into_iter().next().unwrap())?;

    Ok(Label::new(
        relative.repo.unwrap_or_else(|| (&context.repo).into()),
        relative.package.unwrap_or_else(|| context.package.clone()),
        relative.target.unwrap_or_else(|| context.target.clone()),
    ))
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TargetKind<'a> {
    /// A specific target, e.g. `//foo:bar` or `//foo`
    Exact(Cow<'a, str>),
    /// All rules in a package, e.g. `//foo:all`
    AllRules,
    /// All targets in a package, e.g. `//foo:*`
    AllTargets,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TargetPattern<'a, R = Repo<'a>> {
    pub repo: R,
    pub package: Cow<'a, str>,
    pub target_kind: TargetKind<'a>,
    /// If true, this pattern includes all subpackages (the `/...` suffix).
    pub include_subpackages: bool,
}

impl<'a, R> TargetPattern<'a, R> {
    pub fn matches(&self, label: &Label<'a, R>) -> bool
    where
        R: PartialEq,
    {
        if self.repo != label.repo {
            return false;
        }

        if self.include_subpackages {
            if !self.package.is_empty() {
                if !label.package.starts_with(self.package.as_ref()) {
                    return false;
                }
                let remainder = &label.package[self.package.len()..];
                if !remainder.is_empty() && !remainder.starts_with('/') {
                    return false;
                }
            }
        } else {
            if self.package != label.package {
                return false;
            }
        }

        match &self.target_kind {
            TargetKind::Exact(t) => t == &label.target,
            TargetKind::AllRules | TargetKind::AllTargets => true,
        }
    }
}

#[derive(PartialEq, Debug)]
struct RelativeTargetPattern<'a> {
    repo: Option<Repo<'a>>,
    package: Option<Cow<'a, str>>,
    target_kind: TargetKind<'a>,
    include_subpackages: bool,
}

#[allow(clippy::type_complexity)]
fn target_pattern_parser<'a>()
-> impl chumsky::Parser<'a, &'a str, RelativeTargetPattern<'a>, extra::Err<ParseError<'a>>> {
    let repo = repo_parser();
    let package = package_name_parser();
    let target = target_name_parser();

    #[derive(Clone)]
    enum Suffix<'a> {
        AllTargets,
        AllRules,
        Exact(&'a str),
    }

    let target_suffix = just(':').ignore_then(choice((
        just('*').to(Suffix::AllTargets),
        just("all").to(Suffix::AllRules),
        target.map(Suffix::Exact),
    )));

    let package_pattern_parser =
        choice((one_of(r##"! "#$%&'()*+,-.;<=>?@[]^_`{|}"##), alphanumeric()))
            .labelled("valid package character")
            .repeated()
            .at_least(1)
            .to_slice()
            .validate(|x: &str, e, emitter| {
                if x.chars().all(|c: char| c == '.') && x != "..." {
                    emitter.emit(Rich::custom(
                        e.span(),
                        "Package can't include an all-dots segment except ...",
                    ))
                }
                x
            })
            .separated_by(just('/'))
            .to_slice()
            .labelled("package name");

    let pkg_segment = package_pattern_parser.map(|pkg: &str| {
        if pkg.ends_with("/...") || pkg == "..." {
            // Trim the '/...' or '...'
            let trimmed = if pkg == "..." {
                ""
            } else {
                pkg.strip_suffix("/...").unwrap()
            };
            (trimmed, true)
        } else {
            (pkg, false)
        }
    });

    let double_slash_pkg = just("//")
        .ignore_then(pkg_segment.clone().or_not())
        .map(|pkg_opt| match pkg_opt {
            Some((pkg, has_dots)) => (Some(pkg), has_dots),
            None => (Some(""), false),
        });

    let single_slash_dots = just("...").to((Some(""), true));

    (repo.or_not())
        .then(
            choice((
                double_slash_pkg,
                single_slash_dots,
                pkg_segment.map(|(pkg, has_dots)| (Some(pkg), has_dots)),
            ))
            .or_not(),
        )
        .then(target_suffix.or_not())
        .map(
            |((r, p), t): (
                (Option<Repo<'a>>, Option<(Option<&'a str>, bool)>),
                Option<Suffix<'a>>,
            )| {
                let (pkg, include_subpackages) = match p {
                    Some((pkg, has_dots)) => (pkg, has_dots),
                    None => (None, false),
                };

                let target_kind = match (pkg, include_subpackages, t.as_ref()) {
                    // If the target is explicitly specified
                    (_, _, Some(Suffix::AllTargets)) => TargetKind::AllTargets,
                    (_, _, Some(Suffix::AllRules)) => TargetKind::AllRules,
                    (_, _, Some(Suffix::Exact(t))) => TargetKind::Exact(Cow::Borrowed(t)),

                    // Defaults for exact shorthands like //foo/bar or @repo// or //foo/bar/...

                    // If it ends in /... and no explicit target suffix is given, it means :all
                    (_, true, None) => TargetKind::AllRules,

                    // @repo// -> @repo//:repo
                    (Some(""), false, None) if r.is_some() => {
                        let name = r.clone().unwrap().into_name();
                        TargetKind::Exact(name)
                    }

                    // //foo/bar -> //foo/bar:bar
                    (Some(pkg), false, None) if !pkg.is_empty() => {
                        let tgt = pkg.rsplit_once('/').map(|(_, tgt)| tgt).unwrap_or(pkg);
                        TargetKind::Exact(Cow::Borrowed(tgt))
                    }

                    // Default relative
                    (None, false, None) => TargetKind::Exact(Cow::Borrowed("")),
                    (None, false, Some(Suffix::Exact(t))) => TargetKind::Exact(Cow::Borrowed(t)),
                    (Some(""), false, None) => TargetKind::Exact(Cow::Borrowed("")),
                    // If it parsed as a single string segment without : and it didn't match the other rules,
                    // we treat it as an empty target for now, parse_target_pattern turns it into Exact(s).
                    // Or rather, we output Exact("") so it doesn't fail, but parse_target_pattern overrides it.
                    (Some(pkg), false, None) if !pkg.contains('/') => {
                        TargetKind::Exact(Cow::Borrowed(""))
                    }
                    _ => TargetKind::Exact(Cow::Borrowed("")),
                };

                let mut final_pkg = pkg.map(Cow::Borrowed);
                if p.is_none() && t.is_none() {
                    // If there's no struct indicators (//, :), it could be something like "wiz" which parsed as a relative package.
                    // It needs to be correctly identified as the target if it doesn't contain a slash, or package/target if it does.
                    // However, since parse_target_pattern needs to know if it was a relative package vs relative target,
                    // and our definition means "wiz" parses into `package` field, we just accept it here.
                    // The `validation` below needs to NOT fail it if there's no slash and no target_kind explicitly set to something else.
                }

                RelativeTargetPattern {
                    repo: r,
                    package: final_pkg,
                    target_kind,
                    include_subpackages,
                }
            },
        )
        .validate(|rel_pattern: RelativeTargetPattern<'a>, e, emitter| {
            if rel_pattern.repo.is_none()
                && rel_pattern.package.is_none()
                && rel_pattern.target_kind == TargetKind::Exact(Cow::Borrowed(""))
            {
                // empty pattern is invalid
                emitter.emit(Rich::custom(e.span(), "invalid target pattern"));
            }
            rel_pattern
        })
        .labelled("target pattern")
}

/// Parses a target pattern string.
pub fn parse_target_pattern<'a, R>(
    s: &'a str,
    context: &Label<'a, R>,
) -> Result<TargetPattern<'a, Repo<'a>>, ParseError<'a>>
where
    for<'b> &'b R: Into<Repo<'a>>,
{
    let relative = target_pattern_parser()
        .parse(s)
        .into_result()
        .map_err(|errs| errs.into_iter().next().unwrap())?;

    let is_absolute = s.starts_with("//") || s.starts_with('@');

    let package = if is_absolute {
        relative.package.clone().unwrap_or(Cow::Borrowed(""))
    } else {
        match &relative.package {
            Some(p) if context.package.is_empty() => p.clone(),
            Some(p) => {
                if relative.include_subpackages {
                    // For "foo/...", it's a relative path to append to context package
                    Cow::Owned(format!("{}/{}", context.package, p))
                } else if !s.contains(':') && s != "..." {
                    if p.contains('/') {
                        // "foo/wiz"
                        let last_slash = p.rfind('/').unwrap();
                        Cow::Owned(format!("{}/{}", context.package, &p[..last_slash]))
                    } else {
                        // "wiz"
                        context.package.clone()
                    }
                } else if p.is_empty() {
                    context.package.clone()
                } else {
                    Cow::Owned(format!("{}/{}", context.package, p))
                }
            }
            None => context.package.clone(),
        }
    };

    let target_kind = match relative.target_kind {
        // If the relative pattern didn't provide a target and resolved as empty, use context target
        TargetKind::Exact(ref t) if t.is_empty() => TargetKind::Exact(context.target.clone()),
        TargetKind::Exact(ref t)
            if !is_absolute && !s.contains(':') && s != "..." && !s.ends_with("/...") =>
        {
            // It was a relative shorthand like "wiz" or "foo/wiz" but without a colon.
            if let Some(ref rp) = relative.package {
                let tgt = rp
                    .rsplit_once('/')
                    .map(|(_, tgt)| tgt)
                    .unwrap_or(rp.as_ref());
                TargetKind::Exact(Cow::Owned(tgt.to_string()))
            } else {
                TargetKind::Exact(Cow::Owned(s.to_string()))
            }
        }
        other => other,
    };

    Ok(TargetPattern {
        repo: relative.repo.unwrap_or_else(|| (&context.repo).into()),
        package,
        target_kind,
        include_subpackages: relative.include_subpackages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    #[test]
    fn test_display_main_repo() {
        let label = CanonicalLabel::new(MAIN_REPO, "my/pkg", "foo");
        assert_eq!(label.to_string(), "@@//my/pkg:foo");
    }

    #[test]
    fn test_display_apparent_repo() {
        let label = ApparentLabel::new(ApparentRepo::new("my_repo"), "my/pkg", "foo");
        assert_eq!(label.to_string(), "@my_repo//my/pkg:foo");
    }

    #[test]
    fn test_display_canonical_repo() {
        let label = CanonicalLabel::new(CanonicalRepo::new("my_repo_canon"), "my/pkg", "foo");
        assert_eq!(label.to_string(), "@@my_repo_canon//my/pkg:foo");
    }

    #[test]
    fn test_display_shorthand() {
        let label = ApparentLabel::new(ApparentRepo::new("my_repo"), "my/pkg", "pkg");
        assert_eq!(label.to_string(), "@my_repo//my/pkg");
    }

    #[test]
    fn test_display_shorthand_single_segment_package() {
        let label = ApparentLabel::new(ApparentRepo::new("my_repo"), "pkg", "pkg");
        assert_eq!(label.to_string(), "@my_repo//pkg");

        let label = CanonicalLabel::new(MAIN_REPO, "pkg", "pkg");
        assert_eq!(label.to_string(), "@@//pkg");
    }

    #[test]
    fn test_display_shorthand_nopkg() {
        let label = ApparentLabel::new(ApparentRepo::new("my_repo"), "", "pkg");
        assert_eq!(label.to_string(), "@my_repo//:pkg");

        let label = ApparentLabel::new(ApparentRepo::new("my_repo"), "", "my_repo");
        assert_eq!(label.to_string(), "@my_repo//");
    }

    #[test]
    fn test_parse_same_repo() {
        let context = CanonicalLabel::new(CanonicalRepo::new("repo"), "some/pkg", "a/target");
        let label = parse_label("//my/pkg:foo/bar", &context).unwrap();
        assert_eq!(
            label,
            Label::new(
                Repo::Canonical(CanonicalRepo::new("repo")),
                "my/pkg",
                "foo/bar"
            )
        );
    }

    #[test]
    fn test_parse_same_package() {
        let context = CanonicalLabel::new(CanonicalRepo::new("repo"), "my/pkg", "a/target");

        let label = parse_label(":foo/bar", &context).unwrap();
        assert_eq!(
            label,
            Label::new(
                Repo::Canonical(CanonicalRepo::new("repo")),
                "my/pkg",
                "foo/bar"
            )
        );

        let label = parse_label("foo/bar", &context).unwrap();
        assert_eq!(
            label,
            Label::new(
                Repo::Canonical(CanonicalRepo::new("repo")),
                "my/pkg",
                "foo/bar"
            )
        );
    }

    #[test]
    fn test_parse_apparent_repo() {
        let label = parse_label("@my_repo//my/pkg:foo", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(
            label,
            Label::new(
                Repo::Apparent(ApparentRepo::new("my_repo")),
                "my/pkg",
                "foo"
            )
        );
    }

    #[test]
    fn test_parse_canonical_repo() {
        let label = parse_label("@@my_repo_canon//my/pkg:foo", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(
            label,
            Label::new(
                Repo::Canonical(CanonicalRepo::new("my_repo_canon")),
                "my/pkg",
                "foo"
            )
        );
    }

    #[test]
    fn test_parse_main_repo() {
        let context = CanonicalLabel::new(CanonicalRepo::new("repo"), "a/pkg", "target");

        let label = parse_label("@@//other/pkg:foo", &context).unwrap();
        assert_eq!(
            label,
            Label::new(Repo::Canonical(MAIN_REPO), "other/pkg", "foo")
        );
    }

    #[test]
    fn test_parse_shorthand_target() {
        let label = parse_label("//my/pkg", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(
            label,
            Label::new(Repo::Canonical(MAIN_REPO), "my/pkg", "pkg")
        );
    }

    #[test]
    fn test_parse_shorthand_target_root() {
        let label = parse_label("//:foo", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(label, Label::new(Repo::Canonical(MAIN_REPO), "", "foo"));
    }

    #[test]
    fn test_parse_shorthand_target_with_repo() {
        let label = parse_label("@my_repo//my/pkg", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(
            label,
            Label::new(
                Repo::Apparent(ApparentRepo::new("my_repo")),
                "my/pkg",
                "pkg"
            )
        );
    }

    #[test]
    fn test_into_canonical() {
        let mut repo_mapping = HashMap::new();
        repo_mapping.insert("my_repo", "my_repo_canon_v1");

        let label = ApparentLabel::new(ApparentRepo::new("my_repo"), "my/pkg", "foo");
        let canonical_label =
            label.into_canonical(|l| repo_mapping.get(l.as_str()).map(|&s| CanonicalRepo::new(s)));

        assert_eq!(
            canonical_label,
            Some(Label::new(
                CanonicalRepo::new("my_repo_canon_v1"),
                "my/pkg",
                "foo"
            ))
        );

        let label = ApparentLabel::new(ApparentRepo::new("other_repo"), "my/pkg", "foo");
        let unknown_label =
            label.into_canonical(|l| repo_mapping.get(l.as_str()).map(|&s| CanonicalRepo::new(s)));

        assert_eq!(unknown_label, None);
    }

    #[test]
    fn test_relative_target_in_package() {
        let label = Label::new(ApparentRepo::new("repo"), "foo/bar", "quux");
        assert_eq!(
            label.relative(":baz").unwrap(),
            Label::new(Repo::Apparent(ApparentRepo::new("repo")), "foo/bar", "baz")
        );
        assert_eq!(
            label.relative("baz").unwrap(),
            Label::new(Repo::Apparent(ApparentRepo::new("repo")), "foo/bar", "baz")
        );
    }

    #[test]
    fn test_relative_absolute_path() {
        let label = Label::new(ApparentRepo::new("repo"), "foo/bar", "quux");
        assert_eq!(
            label.relative("//other:thing").unwrap(),
            Label::new(Repo::Apparent(ApparentRepo::new("repo")), "other", "thing")
        );
    }

    #[test]
    fn test_relative_absolute_path_with_repo() {
        let label = Label::new(ApparentRepo::new("repo"), "foo/bar", "quux");
        assert_eq!(
            label.relative("@other//pkg:thing").unwrap(),
            Label::new(Repo::Apparent(ApparentRepo::new("other")), "pkg", "thing")
        );

        let label = Label::new(ApparentRepo::new("repo"), "foo/bar", "quux");
        assert_eq!(
            label.relative("@other//").unwrap(), // equivalent to @other//:other
            Label::new(Repo::Apparent(ApparentRepo::new("other")), "", "other")
        );
    }

    #[test]
    fn test_main_repo() {
        assert_eq!(MAIN_REPO.to_string(), "@@");
        assert_eq!(MAIN_REPO_ROOT.to_string(), "@@//");
        assert_eq!(MAIN_REPO_ROOT.repo_name(), "");
    }

    #[test]
    fn test_repo_name() {
        let label_repo = Label::new(CanonicalRepo::new("my_repo"), "pkg", "tgt");
        assert_eq!(label_repo.repo_name(), "my_repo");
    }

    #[test]
    fn test_workspace_root() {
        assert_eq!(MAIN_REPO_ROOT.workspace_root(), "");

        let label_repo = Label::new(CanonicalRepo::new("my_repo"), "pkg", "tgt");
        assert_eq!(label_repo.workspace_root(), "external/my_repo");
    }

    #[test]
    fn test_parse_error_empty() {
        let result = parse_label("", &MAIN_REPO_ROOT);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "found end of input expected label"
        );
    }

    #[test]
    fn test_parse_error_no_double_slash() {
        let result = parse_label("my/pkg:foo", &MAIN_REPO_ROOT);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "found ':' expected valid target character, '/', or end of input"
        );
    }

    #[test]
    fn test_parse_error_empty_target() {
        let result = parse_label("//my/pkg:", &MAIN_REPO_ROOT);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "found end of input expected target name"
        );
    }

    #[test]
    fn test_parse_error_double_slash_in_target() {
        let result = parse_label("//my/pkg:foo//bar", &MAIN_REPO_ROOT);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "found '/' expected valid target character"
        );
    }

    #[test]
    fn test_parse_error_dot_in_target() {
        let result = parse_label("//my/pkg:foo/./bar", &MAIN_REPO_ROOT);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Target can't include . or .."
        );

        let result = parse_label("//my/pkg:../bar", &MAIN_REPO_ROOT);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Target can't include . or .."
        );
    }

    #[test]
    fn test_parse_error_trailing_slash_in_package() {
        let result = parse_label("//my/pkg/:foo", &MAIN_REPO_ROOT);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "found ':' expected valid package character"
        );
    }

    #[test]
    fn test_parse_error_trailing_slash_in_shorthand() {
        let result = parse_label("//my/pkg/", &MAIN_REPO_ROOT);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "found end of input expected valid package character"
        );
    }

    fn arb_repo_name() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_.+-]*"
    }

    fn arb_package_name() -> impl Strategy<Value = String> {
        let component = r##"[a-zA-Z0-9! "#$%&'()*+,.;<=>?@\[\]^_`{|}-]+"##
            .prop_filter("Package component cannot be all dots", |v| {
                !v.chars().all(|c| c == '.')
            });

        prop::collection::vec(component, 0..5).prop_map(|parts| parts.join("/"))
    }

    fn arb_target_name() -> impl Strategy<Value = String> {
        let component = r##"[a-zA-Z0-9!%@^_"#$&'()*+,;<=>?\[\]{|}~.-]+"##
            .prop_filter("Target component cannot be . or ..", |v| {
                v != "." && v != ".."
            });

        prop::collection::vec(component, 1..5).prop_map(|parts| parts.join("/"))
    }

    fn arb_repo() -> impl Strategy<Value = Repo<'static>> {
        prop_oneof![
            arb_repo_name().prop_map(|v| Repo::Apparent(ApparentRepo::new(v))),
            arb_repo_name().prop_map(|v| Repo::Canonical(CanonicalRepo::new(v))),
        ]
    }
    fn arb_label() -> impl Strategy<Value = Label<'static>> {
        (arb_repo(), arb_package_name(), arb_target_name()).prop_map(|(repo, package, target)| {
            Label {
                repo,
                package: package.into(),
                target: target.into(),
            }
        })
    }

    proptest! {
        #[test]
        fn label_to_string_from_string_roundtrip(l in arb_label()) {
            let s = l.to_string();
            let parsed_l = parse_label(&s, &MAIN_REPO_ROOT);
            prop_assert!(parsed_l.is_ok(), "Failed to parse '{}': {}", s, parsed_l.err().unwrap());

            let parsed_l = parsed_l.unwrap();
            prop_assert!(l.repo() == parsed_l.repo());
            prop_assert_eq!(l.package(), parsed_l.package());
            prop_assert_eq!(l.name(), parsed_l.name());
        }
    }

    #[test]
    fn test_debug_apparent_repo() {
        let label = Label::new(ApparentRepo::new("my_repo"), "my/pkg", "foo");
        assert_eq!(format!("{label:?}"), "Label(\"@my_repo//my/pkg:foo\")");
    }

    #[test]
    fn test_debug_canonical_repo() {
        let label = Label::new(CanonicalRepo::new("my_repo_canon"), "my/pkg", "foo");
        assert_eq!(
            format!("{label:?}"),
            "Label(\"@@my_repo_canon//my/pkg:foo\")"
        );
    }

    // --- TargetPattern tests ---

    #[test]
    fn test_target_pattern_exact() {
        let context = MAIN_REPO_ROOT.clone();
        let pat = parse_target_pattern("//foo/bar:wiz", &context).unwrap();
        assert_eq!(pat.repo, Repo::Canonical(MAIN_REPO));
        assert_eq!(pat.package, "foo/bar");
        assert_eq!(pat.target_kind, TargetKind::Exact(Cow::Borrowed("wiz")));
        assert!(!pat.include_subpackages);

        assert!(pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foo/bar", "wiz")));
        assert!(!pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foo/bar", "wizzo")));
        assert!(!pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foo", "wiz")));
    }

    #[test]
    fn test_target_pattern_shorthand_exact() {
        let pat = parse_target_pattern("//foo/bar", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(pat.repo, Repo::Canonical(MAIN_REPO));
        assert_eq!(pat.package, "foo/bar");
        assert_eq!(pat.target_kind, TargetKind::Exact(Cow::Borrowed("bar")));
        assert!(!pat.include_subpackages);
    }

    #[test]
    fn test_target_pattern_all_rules() {
        let pat = parse_target_pattern("//foo/bar:all", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(pat.package, "foo/bar");
        assert_eq!(pat.target_kind, TargetKind::AllRules);
        assert!(!pat.include_subpackages);

        assert!(pat.matches(&Label::new(
            Repo::Canonical(MAIN_REPO),
            "foo/bar",
            "any_target"
        )));
        assert!(!pat.matches(&Label::new(
            Repo::Canonical(MAIN_REPO),
            "foo/bar/baz",
            "any_target"
        )));
    }

    #[test]
    fn test_target_pattern_all_targets() {
        let pat = parse_target_pattern("//foo/bar:*", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(pat.package, "foo/bar");
        assert_eq!(pat.target_kind, TargetKind::AllTargets);
        assert!(!pat.include_subpackages);

        assert!(pat.matches(&Label::new(
            Repo::Canonical(MAIN_REPO),
            "foo/bar",
            "any_target"
        )));
        assert!(pat.matches(&Label::new(
            Repo::Canonical(MAIN_REPO),
            "foo/bar",
            "file.txt"
        )));
    }

    #[test]
    fn test_target_pattern_rules_beneath() {
        let pat = parse_target_pattern("//foo/...", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(pat.package, "foo");
        assert_eq!(pat.target_kind, TargetKind::AllRules);
        assert!(pat.include_subpackages);

        assert!(pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foo", "target")));
        assert!(pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foo/bar", "target")));
        assert!(pat.matches(&Label::new(
            Repo::Canonical(MAIN_REPO),
            "foo/bar/baz",
            "target"
        )));
        // ensure it handles prefix boundaries properly
        assert!(!pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foobar", "target")));
    }

    #[test]
    fn test_target_pattern_rules_beneath_root() {
        let pat = parse_target_pattern("//...", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(pat.package, "");
        assert_eq!(pat.target_kind, TargetKind::AllRules);
        assert!(pat.include_subpackages);

        assert!(pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "", "target")));
        assert!(pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foo", "target")));
        assert!(pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foo/bar", "target")));
    }

    #[test]
    fn test_target_pattern_targets_beneath() {
        let pat = parse_target_pattern("//foo/...:*", &MAIN_REPO_ROOT).unwrap();
        assert_eq!(pat.package, "foo");
        assert_eq!(pat.target_kind, TargetKind::AllTargets);
        assert!(pat.include_subpackages);

        assert!(pat.matches(&Label::new(Repo::Canonical(MAIN_REPO), "foo", "file.txt")));
        assert!(pat.matches(&Label::new(
            Repo::Canonical(MAIN_REPO),
            "foo/bar",
            "file.txt"
        )));
    }

    #[test]
    fn test_target_pattern_relative_beneath() {
        let context = Label::new(Repo::Canonical(MAIN_REPO), "my/pkg", "a_target");
        let pat = parse_target_pattern("foo/...", &context).unwrap();
        assert_eq!(pat.package, "my/pkg/foo");
        assert_eq!(pat.target_kind, TargetKind::AllRules); // the shorthand for ... expands to :all
        assert!(pat.include_subpackages);

        assert!(pat.matches(&Label::new(
            Repo::Canonical(MAIN_REPO),
            "my/pkg/foo",
            "target"
        )));
        assert!(pat.matches(&Label::new(
            Repo::Canonical(MAIN_REPO),
            "my/pkg/foo/bar",
            "target"
        )));
    }

    #[test]
    fn test_target_pattern_relative_exact() {
        let context = Label::new(Repo::Canonical(MAIN_REPO), "my/pkg", "a_target");
        let pat = parse_target_pattern(":wiz", &context).unwrap();
        assert_eq!(pat.package, "my/pkg");
        assert_eq!(pat.target_kind, TargetKind::Exact(Cow::Borrowed("wiz")));
        assert!(!pat.include_subpackages);
    }

    #[test]
    fn test_target_pattern_relative_implied() {
        // Just providing "wiz" as a relative pattern in package "my/pkg"
        let context = Label::new(Repo::Canonical(MAIN_REPO), "my/pkg", "a_target");
        // By Bazel terminology, if there is no `:`, `wiz` in package `my/pkg` means `my/pkg:wiz`
        let pat = parse_target_pattern("wiz", &context).unwrap();
        assert_eq!(pat.package, "my/pkg");
        assert_eq!(pat.target_kind, TargetKind::Exact(Cow::Borrowed("wiz")));
        assert!(!pat.include_subpackages);
    }
}
