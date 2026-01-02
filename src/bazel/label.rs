// src/bazel/label.rs

use std::fmt;

/// An empty string means the main repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApparentRepo<S>(S);
impl<S: AsRef<str>> ApparentRepo<S> {
    pub const fn new(name: S) -> Self {
        Self(name)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl<S> ApparentRepo<S> {
    pub fn into_name(self) -> S {
        self.0
    }
}

impl<S: AsRef<str>> AsRef<str> for ApparentRepo<S> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<S: fmt::Display> fmt::Display for ApparentRepo<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalRepo<S>(S);
impl<S: AsRef<str>> CanonicalRepo<S> {
    pub const fn new(name: S) -> Self {
        Self(name)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl<S> CanonicalRepo<S> {
    pub fn into_name(self) -> S {
        self.0
    }
}

impl<S: AsRef<str>> AsRef<str> for CanonicalRepo<S> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<S: fmt::Display> fmt::Display for CanonicalRepo<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@@{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub enum Repo<S> {
    Apparent(ApparentRepo<S>),
    Canonical(CanonicalRepo<S>),
}

impl<S> Repo<S> {
    pub fn into_name(self) -> S {
        match self {
            Repo::Apparent(r) => r.into_name(),
            Repo::Canonical(r) => r.into_name(),
        }
    }
}

impl<S: fmt::Display> fmt::Display for Repo<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Repo::Apparent(r) => r.fmt(f),
            Repo::Canonical(r) => r.fmt(f),
        }
    }
}

impl<S, T> PartialEq<Repo<T>> for Repo<S>
where
    S: PartialEq<T>,
{
    fn eq(&self, other: &Repo<T>) -> bool {
        match (self, other) {
            (Repo::Apparent(ApparentRepo(l)), Repo::Apparent(ApparentRepo(r))) => l == r,
            (Repo::Canonical(CanonicalRepo(l)), Repo::Canonical(CanonicalRepo(r))) => l == r,
            _ => false,
        }
    }
}

impl<S> Eq for Repo<S> where S: Eq {}

impl<S> AsRef<str> for Repo<S>
where
    S: AsRef<str>,
{
    fn as_ref(&self) -> &str {
        match self {
            Repo::Apparent(r) => r.as_ref(),
            Repo::Canonical(r) => r.as_ref(),
        }
    }
}

impl<S> From<CanonicalRepo<S>> for Repo<S> {
    fn from(r: CanonicalRepo<S>) -> Self {
        Repo::Canonical(r)
    }
}

impl<S> From<ApparentRepo<S>> for Repo<S> {
    fn from(r: ApparentRepo<S>) -> Self {
        Repo::Apparent(r)
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
pub struct Label<S, R = Repo<S>> {
    /// The repository name, e.g., `my_repo`.
    pub repo: R,
    /// The package path, e.g., `my/package`.
    pub package: S,
    /// The target name, e.g., `my_target`.
    pub target: S,
}

/// A Bazel label, identifying a canonical repo target.
///
/// A canonical-repo label has the form:
///
/// `@@[repo_name]//[package_path]:[target_name]`
///
/// See https://bazel.build/concepts/labels
pub type CanonicalLabel<S> = Label<S, CanonicalRepo<S>>;

/// A Bazel label, identifying an apparent repo target.
///
/// An apparent-repo label has the form:
///
/// `@[repo_name]//[package_path]:[target_name]`
///
/// See https://bazel.build/concepts/labels
pub type ApparentLabel<S> = Label<S, ApparentRepo<S>>;

/// The well-known 'main' repo representing the main workspace.
/// ```
/// use crate::bazel::label::MAIN_REPO;
///
/// assert_eq!(MAIN_REPO.to_string(), "@@");
/// ```
pub const MAIN_REPO: CanonicalRepo<&'static str> = CanonicalRepo::new("");

/// A label representing the well-known 'main' repo root.  This is the workspace root.
/// ```
/// use crate::bazel::label::MAIN_REPO_ROOT;
///
/// assert_eq!(MAIN_REPO_ROOT.to_string(), "@@//");
/// ```
pub const MAIN_REPO_ROOT: CanonicalLabel<&'static str> = CanonicalLabel::new(MAIN_REPO, "", "");

impl<S, R> Label<S, R> {
    /// Creates a new `Label`.
    pub const fn new(repo: R, package: S, target: S) -> Self {
        Self {
            repo: repo,
            package: package,
            target: target,
        }
    }
}

impl<S> ApparentLabel<S> {
    /// Converts this apparent label to a canonical label, given a repo mapping.
    ///
    /// use crate::bazel::label::{ApparentLabel, ApparentRepo, CanonicalRepo};
    /// use std::collections::HashMap;
    ///
    /// let mut repo_mapping = HashMap::new();
    /// repo_mapping.insert("my_repo", "my_repo_canon");
    ///
    /// let apparent_label = ApparentLabel::new(ApparentRepo::new("my_repo"), "my/package", "my_target");
    ///
    /// let canonical_label = apparent_label.to_canonical(|l| repo_mapping.get(l.as_str()).map(|&s| CanonicalRepo::new(s))).unwrap();
    /// assert_eq!(canonical_label.to_string(), "@@my_repo_canon//my/package:my_target");
    pub fn to_canonical<F, T>(self, func: F) -> Option<CanonicalLabel<T>>
    where
        F: FnOnce(&ApparentRepo<S>) -> Option<CanonicalRepo<T>>,
        T: From<S>,
    {
        func(&self.repo).map(|canonical_name| CanonicalLabel {
            repo: canonical_name,
            package: self.package.into(),
            target: self.target.into(),
        })
    }
}

impl<S> Label<S, Repo<S>> {
    /// Converts this apparent label to a canonical label, given a repo mapping.
    ///
    /// ```
    /// use crate::bazel::label::ApparentLabel;
    /// use std::collections::HashMap;
    ///
    /// let mut repo_mapping = HashMap::new();
    /// repo_mapping.insert("my_repo", "my_repo_canon");
    ///
    /// let label: Label = "@my_repo//my/package:my_target".parse();
    ///
    /// let canonical_label = label.to_canonical(|l| repo_mapping.get(l)).unwrap();
    /// assert_eq!(canonical_label.to_string(), "@@my_repo_canon//my/package:my_target");
    ///
    /// let label2: Label = "@@canon_repo//my/package:my_target".parse();
    ///
    /// let canonical_label = label.to_canonical(|l| repo_mapping.get(l)).unwrap();
    /// assert_eq!(canonical_label.to_string(), "@@canon_repo//my/package:my_target");
    /// ```
    pub fn to_canonical<F>(self, func: F) -> Option<CanonicalLabel<S>>
    where
        F: FnOnce(&ApparentRepo<S>) -> Option<CanonicalRepo<S>>,
    {
        match self.repo {
            Repo::Apparent(r) => func(&r),
            Repo::Canonical(r) => Some(r),
        }
        .map(|repo| CanonicalLabel {
            repo: repo,
            package: self.package,
            target: self.target,
        })
    }
}

impl<S: fmt::Display, R: fmt::Display> fmt::Debug for Label<S, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Label(\"{}//{}:{}\")",
            self.repo, self.package, self.target
        )
    }
}

impl<S: AsRef<str>, R: fmt::Display + AsRef<str>> fmt::Display for Label<S, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.package.as_ref() == "" && self.target.as_ref() == self.repo_name() {
            return write!(f, "{}//", self.repo);
        }
        if let Some((_, last)) = self.package.as_ref().rsplit_once('/') {
            if last == self.target.as_ref() {
                return write!(f, "{}//{}", self.repo, self.package.as_ref());
            }
        }
        write!(
            f,
            "{}//{}:{}",
            self.repo,
            self.package.as_ref(),
            self.target.as_ref()
        )
    }
}

impl<S: AsRef<str>, R> Label<S, R> {
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

impl<S, R> Label<S, R> {
    /// Creates a new label in the same package as this label.
    /// Corresponds to `Label.same_package_label()` in Starlark.
    pub fn same_package_label(&self, name: S) -> Label<S, R>
    where
        R: Clone,
        S: Clone,
    {
        Label {
            repo: self.repo.clone(),
            package: self.package.clone(),
            target: name,
        }
    }

    /// Resolves a relative label string.
    /// Corresponds to `Label.relative()` in Starlark.
    pub fn relative<'a>(
        &'a self,
        rel_path: &'a str,
    ) -> Result<Label<&'a str, Repo<&'a str>>, ParseError<'a>>
    where
        R: Clone,
        Repo<&'a str>: From<R>,
        S: AsRef<str>,
    {
        parse_label(rel_path, self)
    }
}

impl<S, R: AsRef<str>> Label<S, R> {
    /// The name of the repository in which this target is defined.
    /// An alias for `repo()`.
    pub fn repo_name(&self) -> &str {
        self.repo.as_ref()
    }

    /// The execution-time path of the workspace in which this target is defined.
    /// Corresponds to `Label.workspace_root` in Starlark.
    pub fn workspace_root(&self) -> String {
        if self.repo.as_ref().is_empty() {
            "".to_string()
        } else {
            format!("external/{}", self.repo_name())
        }
    }
}

/// The pieces of a parsed label, used in intermediate calculations.
#[derive(PartialEq, Debug)]
struct RelativeLabel<S> {
    repo: Option<Repo<S>>,
    package: Option<S>,
    target: Option<S>,
}

use chumsky::prelude::*;

type ParseError<'a> = Rich<'a, char>;

fn parser<'a>()
-> impl chumsky::Parser<'a, &'a str, RelativeLabel<&'a str>, extra::Err<ParseError<'a>>> {
    // See https://bazel.build/concepts/labels#labels-lexical-specification

    let alphanumeric = any()
        .filter(|c: &char| c.is_ascii_alphanumeric())
        .labelled("alphanumeric");

    // "Target names must be composed entirely of characters drawn from the set a–z, A–Z, 0–9, and the punctuation symbols !%-@^_"#$&'()*-+,;<=>?[]{|}~/.."
    // Can't start or end with "/", also can't contain "." or ".." path segments.
    let target = choice((one_of(r##"!%@^_"#$&'()*-+,;<=>?[]{|}~."##), alphanumeric))
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
        .labelled("target name");

    let package = choice((one_of(r##"! "#$%&'()*+,-.;<=>?@[]^_`{|}"##), alphanumeric))
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
        .labelled("package name");

    let repo_name = choice((one_of(r##"+_.-"##), alphanumeric))
        .labelled("valid repo character")
        .repeated()
        .to_slice()
        .labelled("repository name");

    let repo = choice((
        just("@@")
            .ignore_then(repo_name)
            .map(|s| Repo::Canonical(CanonicalRepo::new(s)))
            .labelled("canonical repo"),
        just('@')
            .ignore_then(repo_name)
            .map(|s| Repo::Apparent(ApparentRepo::new(s)))
            .labelled("apparent repo"),
    ));

    (repo.or_not())
        .then(just("//").ignore_then(package).map(Some))
        .then((just(':').ignore_then(target)).or_not())
        .map(
            |((r, p), t): ((Option<Repo<&str>>, Option<&str>), Option<&str>)| match ((r, p), t) {
                // Expand shorthand: @repo// -> @repo//:repo
                ((Some(repo), Some(pkg)), None) if pkg == "" => {
                    ((Some(repo.clone()), Some(pkg)), Some(repo.into_name()))
                }
                // Expand shorthand: @repo//my/pkg -> @repo//my/pkg:pkg
                ((r, Some(pkg)), None) => {
                    let tgt = pkg.rsplit_once('/').map(|(_, tgt)| tgt).unwrap_or(pkg);
                    ((r, Some(pkg)), Some(tgt))
                }
                v => v,
            },
        )
        .validate(|((r, p), t), e, emitter| {
            if r.is_none() && p.is_none() && t.is_none() {
                emitter.emit(Rich::custom(e.span(), "invalid label"));
            }
            ((r, p), t)
        })
        .or(just(':')
            .or_not()
            .ignore_then(target)
            .map(|t| ((None, None), Some(t))))
        .labelled("label")
        .map(|((repo, package), target)| RelativeLabel {
            repo,
            package,
            target,
        })
}

/// Parses a label string.
// TODO: Remove the `R: Clone` constraint
pub fn parse_label<'a, S, R>(
    s: &'a str,
    context: &'a Label<S, R>,
) -> Result<Label<&'a str, Repo<&'a str>>, ParseError<'a>>
where
    R: Into<Repo<&'a str>> + Clone,
    S: AsRef<str>,
{
    //log::debug!("Label parser grammar is {}", parser().debug().to_ebnf());

    let relative = parser()
        .parse(s)
        .into_result()
        .map_err(|errs| errs.into_iter().next().unwrap())?;

    Ok(Label::new(
        relative.repo.unwrap_or_else(|| context.repo.clone().into()),
        relative.package.unwrap_or_else(|| context.package.as_ref()),
        relative.target.unwrap_or_else(|| context.target.as_ref()),
    ))
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
    fn test_to_canonical() {
        let mut repo_mapping = HashMap::new();
        repo_mapping.insert("my_repo", "my_repo_canon_v1");

        let label = ApparentLabel::new(ApparentRepo::new("my_repo"), "my/pkg", "foo");
        let canonical_label =
            label.to_canonical(|l| repo_mapping.get(l.as_str()).map(|&s| CanonicalRepo::new(s)));

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
            label.to_canonical(|l| repo_mapping.get(l.as_str()).map(|&s| CanonicalRepo::new(s)));

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

    fn arb_repo() -> impl Strategy<Value = Repo<String>> {
        prop_oneof![
            arb_repo_name().prop_map(|v| Repo::Apparent(ApparentRepo::new(v))),
            arb_repo_name().prop_map(|v| Repo::Canonical(CanonicalRepo::new(v))),
        ]
    }
    fn arb_label() -> impl Strategy<Value = Label<String>> {
        (arb_repo(), arb_package_name(), arb_target_name()).prop_map(|(repo, package, target)| {
            Label {
                repo,
                package,
                target,
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
        assert_eq!(format!("{:?}", label), "Label(\"@my_repo//my/pkg:foo\")");
    }

    #[test]
    fn test_debug_canonical_repo() {
        let label = Label::new(CanonicalRepo::new("my_repo_canon"), "my/pkg", "foo");
        assert_eq!(
            format!("{:?}", label),
            "Label(\"@@my_repo_canon//my/pkg:foo\")"
        );
    }
}
