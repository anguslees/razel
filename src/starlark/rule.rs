use crate::bazel::label::CanonicalLabel;
use crate::bazel::repo::{Repository, RepositoryMapping};
use crate::bazel::rule::{
    AttributeDependency, AttributeSchema, AttributeType, AttributeValue, AttributeValuePart,
    DirectAttributeValue, RuleClassId, RuleClassKind, RuleClassSpec, Selector,
    base_rule_attributes,
};
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::collections::{SmallMap, StarlarkHasher};
use starlark::environment::{FrozenModule, GlobalsBuilder};
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_module;
use starlark::typing::Ty;
use starlark::values::dict::DictRef;
use starlark::values::function::FUNCTION_TYPE;
use starlark::values::list::ListRef;
use starlark::values::{
    Freeze, FreezeResult, Freezer, FrozenValue, Heap, NoSerialize, OwnedFrozenValue, StarlarkValue,
    Trace, UnpackValue, Value, ValueLike, starlark_value,
};
use starlark::{
    register_avalue_simple_frozen, register_ty_starlark_value, starlark_complex_values,
    starlark_simple_value,
};
use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::Hash;
#[derive(Debug, ProvidesStaticType)]
pub(crate) struct LabelResolver<'a> {
    repository: &'a Repository<'static>,
    context: CanonicalLabel<'a>,
}

impl<'a> LabelResolver<'a> {
    pub fn new(repository: &'a Repository<'static>, context: CanonicalLabel<'a>) -> Self {
        Self {
            repository,
            context,
        }
    }

    pub fn resolve(&self, text: &str) -> anyhow::Result<CanonicalLabel<'static>> {
        let parsed = crate::bazel::label::parse_label(text, &self.context)
            .map_err(|error| anyhow::anyhow!("invalid label `{text}`: {error}"))?;
        self.repository
            .resolve_label(parsed)
            .map(CanonicalLabel::into_owned)
            .ok_or_else(|| anyhow::anyhow!("cannot resolve repository mapping in label `{text}`"))
    }

    pub fn context(&self) -> &CanonicalLabel<'a> {
        &self.context
    }
}

#[derive(Debug, Clone)]
struct OwnedLabelResolver {
    repository: RepositoryMapping<'static>,
    context: CanonicalLabel<'static>,
}

impl OwnedLabelResolver {
    fn new(repository: &Repository<'static>, context: &CanonicalLabel<'_>) -> Self {
        Self {
            repository: repository.mapping().clone(),
            context: context.clone().into_owned(),
        }
    }

    fn resolve(&self, text: &str) -> anyhow::Result<CanonicalLabel<'static>> {
        let parsed = crate::bazel::label::parse_label(text, &self.context)
            .map_err(|error| anyhow::anyhow!("invalid label `{text}`: {error}"))?;
        self.repository
            .resolve_label(parsed)
            .map(CanonicalLabel::into_owned)
            .ok_or_else(|| anyhow::anyhow!("cannot resolve repository mapping in label `{text}`"))
    }
}

#[derive(Debug, ProvidesStaticType, Allocative)]
pub(crate) struct BzlExtra<'a> {
    #[allocative(skip)]
    resolver: LabelResolver<'a>,
}

impl<'a> BzlExtra<'a> {
    pub fn new(resolver: LabelResolver<'a>) -> Self {
        Self { resolver }
    }

    fn from_eval<'v, 'e>(eval: &'v Evaluator<'_, '_, 'e>) -> starlark::Result<&'v BzlExtra<'e>> {
        eval.extra
            .and_then(|extra| extra.downcast_ref::<BzlExtra<'e>>())
            .ok_or_else(|| starlark_error("rule() is only available while evaluating a .bzl file"))
    }
}

fn resolver_from_eval<'v, 'e>(
    eval: &'v Evaluator<'_, '_, 'e>,
) -> starlark::Result<&'v LabelResolver<'e>> {
    if let Some(extra) = eval
        .extra
        .and_then(|extra| extra.downcast_ref::<BzlExtra<'e>>())
    {
        return Ok(&extra.resolver);
    }
    if let Some(extra) = eval
        .extra
        .and_then(|extra| extra.downcast_ref::<crate::starlark::globals::build::BuildExtra<'e>>())
    {
        return Ok(&extra.resolver);
    }
    Err(starlark_error("label context is unavailable"))
}

#[derive(Debug, Clone, Trace, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct StarlarkLabel<'v> {
    #[trace(unsafe_ignore)]
    label: CanonicalLabel<'v>,
}

starlark_complex_values!(StarlarkLabel);
register_ty_starlark_value!(StarlarkLabel<'_>);

impl fmt::Display for StarlarkLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[starlark_value(type = "Label")]
impl<'v> StarlarkValue<'v> for StarlarkLabel<'v> {
    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        self.label.hash(hasher);
        Ok(())
    }

    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(unpack_starlark_label(other).is_some_and(|other| self.label == *other))
    }
}

#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct FrozenStarlarkLabel {
    label: CanonicalLabel<'static>,
}

impl fmt::Display for FrozenStarlarkLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[starlark_value(type = "Label")]
impl<'v> StarlarkValue<'v> for FrozenStarlarkLabel {
    type Canonical = StarlarkLabel<'v>;

    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        self.label.hash(hasher);
        Ok(())
    }

    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(unpack_starlark_label(other).is_some_and(|other| self.label == *other))
    }
}

impl Freeze for StarlarkLabel<'_> {
    type Frozen = FrozenStarlarkLabel;

    fn freeze(self, _freezer: &Freezer) -> FreezeResult<Self::Frozen> {
        Ok(FrozenStarlarkLabel {
            label: self.label.into_owned(),
        })
    }
}

fn unpack_starlark_label<'v>(value: Value<'v>) -> Option<&'v CanonicalLabel<'v>> {
    StarlarkLabel::from_value(value).map(|label| match label {
        starlark::__macro_refs::Either::Left(label) => &label.label,
        starlark::__macro_refs::Either::Right(label) => &label.label,
    })
}

#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, Allocative)]
struct LabelCallable {
    #[allocative(skip)]
    resolver: OwnedLabelResolver,
}

starlark_simple_value!(LabelCallable);

impl fmt::Display for LabelCallable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<built-in Label>")
    }
}

#[starlark_value(type = FUNCTION_TYPE)]
impl<'v> StarlarkValue<'v> for LabelCallable {
    fn name_for_call_stack(&self, _me: Value<'v>) -> String {
        "Label".to_owned()
    }

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        args.no_named_args()?;
        let value = args.positional1(eval.heap())?;
        let text = value
            .unpack_str()
            .ok_or_else(|| starlark_error("Label() expects a string"))?;
        let label = self.resolver.resolve(text).map_err(starlark_error)?;
        Ok(eval.heap().alloc_complex(StarlarkLabel { label }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Allocative)]
enum RawValue {
    None,
    Bool(bool),
    Int(i64),
    String(String),
    Label(CanonicalLabel<'static>),
    List(Vec<RawValue>),
    Dict(Vec<(RawValue, RawValue)>),
}

impl RawValue {
    fn from_value(value: Value<'_>) -> starlark::Result<Self> {
        if SelectorExpression::from_value(value).is_some() {
            return Err(starlark_error("nested select() values are not supported"));
        }
        if value.is_none() {
            return Ok(Self::None);
        }
        if let Some(value) = value.unpack_bool() {
            return Ok(Self::Bool(value));
        }
        if let Some(value) = i64::unpack_value(value)? {
            return Ok(Self::Int(value));
        }
        if let Some(value) = value.unpack_str() {
            return Ok(Self::String(value.to_owned()));
        }
        if let Some(value) = unpack_starlark_label(value) {
            return Ok(Self::Label(value.clone().into_owned()));
        }
        if let Some(list) = ListRef::from_value(value) {
            return list
                .iter()
                .map(Self::from_value)
                .collect::<starlark::Result<Vec<_>>>()
                .map(Self::List);
        }
        if let Some(dict) = DictRef::from_value(value) {
            return dict
                .iter()
                .map(|(key, value)| Ok((Self::from_value(key)?, Self::from_value(value)?)))
                .collect::<starlark::Result<Vec<_>>>()
                .map(Self::Dict);
        }
        Err(starlark_error(format!(
            "unsupported loading-time value of type `{}`",
            value.get_type()
        )))
    }
}

#[derive(Debug, Clone, Allocative)]
struct SelectorBranches {
    conditions: BTreeMap<CanonicalLabel<'static>, Option<RawValue>>,
    default: Option<Option<RawValue>>,
    no_match_error: String,
}

#[derive(Debug, Clone, Allocative)]
enum SelectorTerm {
    Direct(RawValue),
    Select(SelectorBranches),
}

#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, Allocative)]
struct SelectorExpression {
    terms: Vec<SelectorTerm>,
}

starlark_simple_value!(SelectorExpression);

impl fmt::Display for SelectorExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<select expression>")
    }
}

fn selector_terms(value: Value<'_>) -> starlark::Result<Vec<SelectorTerm>> {
    if let Some(selector) = SelectorExpression::from_value(value) {
        Ok(selector.terms.clone())
    } else {
        Ok(vec![SelectorTerm::Direct(RawValue::from_value(value)?)])
    }
}

#[starlark_value(type = "select")]
impl<'v> StarlarkValue<'v> for SelectorExpression {
    fn add(&self, rhs: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        Some(selector_terms(rhs).map(|mut rhs| {
            let mut terms = self.terms.clone();
            terms.append(&mut rhs);
            heap.alloc(Self { terms })
        }))
    }

    fn radd(&self, lhs: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        Some(selector_terms(lhs).map(|mut lhs| {
            lhs.extend(self.terms.clone());
            heap.alloc(Self { terms: lhs })
        }))
    }
}

#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct AttrDescriptor {
    #[allocative(skip)]
    schema: AttributeSchema,
}

starlark_simple_value!(AttrDescriptor);

impl fmt::Display for AttrDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<attr.{:?}>", self.schema.attribute_type)
    }
}

#[starlark_value(type = "attribute")]
impl<'v> StarlarkValue<'v> for AttrDescriptor {}

pub(crate) fn add_context_globals(
    builder: &mut GlobalsBuilder,
    repository: &Repository<'static>,
    context: &CanonicalLabel<'_>,
) {
    builder.set(
        "Label",
        LabelCallable {
            resolver: OwnedLabelResolver::new(repository, context),
        },
    );
    common_globals(builder);
}

#[starlark_module]
fn common_globals(builder: &mut GlobalsBuilder) {
    fn select<'v>(
        mapping: DictRef<'v>,
        #[starlark(default = "")] no_match_error: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<SelectorExpression> {
        let resolver = resolver_from_eval(eval)?;
        let mut conditions = BTreeMap::new();
        let mut default = None;
        for (key, value) in mapping.iter() {
            let (condition, is_default) = if let Some(key) = key.unpack_str() {
                (
                    resolver.resolve(key).map_err(starlark_error)?,
                    key == "//conditions:default",
                )
            } else if let Some(key) = unpack_starlark_label(key) {
                (
                    key.clone().into_owned(),
                    key.repo.as_str().is_empty()
                        && key.package() == "conditions"
                        && key.name() == "default",
                )
            } else {
                return Err(starlark_error(
                    "select() keys must be strings or Label values",
                ));
            };
            let value = if value.is_none() {
                None
            } else {
                Some(RawValue::from_value(value)?)
            };
            if is_default {
                if default.is_some() {
                    return Err(starlark_error(format!(
                        "duplicate select() condition `{condition}`"
                    )));
                }
                default = Some(value);
            } else {
                match conditions.entry(condition) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(value);
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        return Err(starlark_error(format!(
                            "duplicate select() condition `{}`",
                            entry.key()
                        )));
                    }
                }
            }
        }
        if conditions.is_empty() && default.is_none() {
            return Err(starlark_error("select() requires at least one condition"));
        }
        Ok(SelectorExpression {
            terms: vec![SelectorTerm::Select(SelectorBranches {
                conditions,
                default,
                no_match_error: no_match_error.to_owned(),
            })],
        })
    }
}

pub(crate) fn add_attr_namespace(builder: &mut GlobalsBuilder) {
    builder.namespace("attr", attr_globals);
}

#[starlark_module]
fn attr_globals(builder: &mut GlobalsBuilder) {
    fn r#bool<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::Bool, kwargs, eval)
    }

    fn int<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::Int, kwargs, eval)
    }

    fn int_list<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::IntList, kwargs, eval)
    }

    fn string<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::String, kwargs, eval)
    }

    fn string_list<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::StringList, kwargs, eval)
    }

    fn string_list_dict<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::StringListDict, kwargs, eval)
    }

    fn string_dict<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::StringDict, kwargs, eval)
    }

    fn label<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::Label, kwargs, eval)
    }

    fn label_list<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::LabelList, kwargs, eval)
    }

    fn label_keyed_string_dict<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::LabelKeyedStringDict, kwargs, eval)
    }

    fn label_list_dict<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::LabelListDict, kwargs, eval)
    }

    fn string_keyed_label_dict<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::StringKeyedLabelDict, kwargs, eval)
    }

    fn output<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::Output, kwargs, eval)
    }

    fn output_list<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<AttrDescriptor> {
        make_attr(AttributeType::OutputList, kwargs, eval)
    }
}

fn make_attr<'v>(
    attribute_type: AttributeType,
    kwargs: SmallMap<&str, Value<'v>>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<AttrDescriptor> {
    let mut default_value = None;
    let mut mandatory = false;
    let mut allow_files = None;
    let mut allow_empty = None;

    for (name, value) in kwargs {
        match name {
            "default" => default_value = Some(value),
            "mandatory" => {
                mandatory = value
                    .unpack_bool()
                    .ok_or_else(|| starlark_error("mandatory must be a bool"))?;
            }
            "doc" => {
                if !value.is_none() && value.unpack_str().is_none() {
                    return Err(starlark_error("doc must be a string or None"));
                }
            }
            "allow_empty" => {
                let supported = matches!(
                    attribute_type,
                    AttributeType::String
                        | AttributeType::IntList
                        | AttributeType::StringList
                        | AttributeType::StringListDict
                        | AttributeType::StringDict
                        | AttributeType::LabelList
                        | AttributeType::LabelKeyedStringDict
                        | AttributeType::LabelListDict
                        | AttributeType::StringKeyedLabelDict
                        | AttributeType::OutputList
                );
                let parsed = value
                    .unpack_bool()
                    .ok_or_else(|| starlark_error("allow_empty must be a bool"))?;
                if !supported {
                    return Err(starlark_error(format!(
                        "allow_empty is not supported by attr.{attribute_type:?}"
                    )));
                }
                allow_empty = Some(parsed);
            }
            "allow_files" => {
                if !matches!(
                    attribute_type,
                    AttributeType::Label
                        | AttributeType::LabelList
                        | AttributeType::LabelKeyedStringDict
                        | AttributeType::LabelListDict
                        | AttributeType::StringKeyedLabelDict
                ) {
                    return Err(starlark_error(
                        "allow_files is only valid for label attributes",
                    ));
                }
                if value.is_none() {
                    allow_files = None;
                } else if let Some(value) = value.unpack_bool() {
                    allow_files = Some(value);
                } else if is_empty_list(value) {
                    allow_files = Some(false);
                } else {
                    return Err(starlark_error(
                        "file-type allow_files filters are not supported yet",
                    ));
                }
            }
            "cfg" | "materializer" => require_none(name, value)?,
            "providers" | "aspects" | "allow_rules" | "flags" | "values" => {
                require_empty_list(name, value)?;
            }
            "allow_single_file" | "executable" | "for_dependency_resolution" => {
                require_false(name, value)?;
            }
            _ => return Err(starlark_error(format!("unsupported attr option `{name}`"))),
        }
    }

    let dependency = match attribute_type {
        AttributeType::Label
        | AttributeType::LabelList
        | AttributeType::LabelKeyedStringDict
        | AttributeType::LabelListDict
        | AttributeType::StringKeyedLabelDict => AttributeDependency::Dependency,
        AttributeType::Output | AttributeType::OutputList => AttributeDependency::Output,
        AttributeType::Bool
        | AttributeType::Int
        | AttributeType::IntList
        | AttributeType::String
        | AttributeType::StringList
        | AttributeType::StringListDict
        | AttributeType::StringDict => AttributeDependency::NoDependency,
    };
    let resolver = resolver_from_eval(eval)?;
    let default = match default_value {
        Some(value) if value.is_none() => None,
        Some(value) => Some(convert_direct(
            attribute_type,
            RawValue::from_value(value)?,
            resolver,
        )?),
        None => type_default(attribute_type),
    };
    let schema = AttributeSchema {
        name: String::new(),
        public: true,
        attribute_type,
        default,
        mandatory,
        dependency,
        configurable: !matches!(
            attribute_type,
            AttributeType::Output | AttributeType::OutputList
        ),
        selector_conditions_are_dependencies: !matches!(
            attribute_type,
            AttributeType::Output | AttributeType::OutputList
        ),
        allow_files,
        allow_empty,
    };
    if schema.allow_empty == Some(false)
        && schema.default.as_ref().is_some_and(direct_value_is_empty)
    {
        return Err(starlark_error("attribute default may not be empty"));
    }
    Ok(AttrDescriptor { schema })
}

fn type_default(attribute_type: AttributeType) -> Option<DirectAttributeValue> {
    match attribute_type {
        AttributeType::Bool => Some(DirectAttributeValue::Bool(false)),
        AttributeType::Int => Some(DirectAttributeValue::Int(0)),
        AttributeType::IntList => Some(DirectAttributeValue::IntList(Vec::new())),
        AttributeType::String => Some(DirectAttributeValue::String(String::new())),
        AttributeType::StringList => Some(DirectAttributeValue::StringList(Vec::new())),
        AttributeType::StringListDict => {
            Some(DirectAttributeValue::StringListDict(BTreeMap::new()))
        }
        AttributeType::StringDict => Some(DirectAttributeValue::StringDict(BTreeMap::new())),
        AttributeType::Label => None,
        AttributeType::LabelList => Some(DirectAttributeValue::LabelList(Vec::new())),
        AttributeType::LabelKeyedStringDict => {
            Some(DirectAttributeValue::LabelKeyedStringDict(BTreeMap::new()))
        }
        AttributeType::LabelListDict => Some(DirectAttributeValue::LabelListDict(BTreeMap::new())),
        AttributeType::StringKeyedLabelDict => {
            Some(DirectAttributeValue::StringKeyedLabelDict(BTreeMap::new()))
        }
        AttributeType::Output => None,
        AttributeType::OutputList => Some(DirectAttributeValue::OutputList(Vec::new())),
    }
}

fn is_empty_list(value: Value<'_>) -> bool {
    ListRef::from_value(value).is_some_and(|value| value.is_empty())
}

fn require_none(name: &str, value: Value<'_>) -> starlark::Result<()> {
    if value.is_none() {
        Ok(())
    } else {
        Err(starlark_error(format!(
            "non-default `{name}` is not supported"
        )))
    }
}

fn require_false(name: &str, value: Value<'_>) -> starlark::Result<()> {
    if value.unpack_bool() == Some(false) {
        Ok(())
    } else {
        Err(starlark_error(format!(
            "non-default `{name}` is not supported"
        )))
    }
}

fn require_empty_list(name: &str, value: Value<'_>) -> starlark::Result<()> {
    if is_empty_list(value) {
        Ok(())
    } else {
        Err(starlark_error(format!(
            "non-default `{name}` is not supported"
        )))
    }
}

trait ExportName: fmt::Debug + Allocative + 'static {
    fn get(&self) -> Option<&str>;
    fn export(&self, name: &str) -> starlark::Result<()>;
}

#[derive(Debug, Trace, ProvidesStaticType, Allocative)]
struct MutableExportName {
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    value: OnceCell<String>,
}

impl ExportName for MutableExportName {
    fn get(&self) -> Option<&str> {
        self.value.get().map(String::as_str)
    }

    fn export(&self, name: &str) -> starlark::Result<()> {
        match self.value.get() {
            None => self
                .value
                .set(name.to_owned())
                .map_err(|_| starlark_error("failed to export rule name")),
            Some(existing) if existing == name => Ok(()),
            Some(existing) => Err(starlark_error(format!(
                "rule already exported as `{existing}`, cannot re-export as `{name}`"
            ))),
        }
    }
}

#[derive(Debug, Trace, ProvidesStaticType, Allocative)]
struct FrozenExportName {
    #[trace(unsafe_ignore)]
    value: Option<String>,
}

impl ExportName for FrozenExportName {
    fn get(&self) -> Option<&str> {
        self.value.as_deref()
    }

    fn export(&self, name: &str) -> starlark::Result<()> {
        match self.value.as_deref() {
            Some(existing) if existing == name => Ok(()),
            Some(existing) => Err(starlark_error(format!(
                "rule already exported as `{existing}`, cannot re-export as `{name}`"
            ))),
            None => Err(starlark_error("cannot export an unnamed frozen rule")),
        }
    }
}

#[derive(Debug, Trace, ProvidesStaticType, NoSerialize, Allocative)]
struct RuleCallableGen<V, E> {
    implementation: V,
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    attributes: Vec<AttributeSchema>,
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    defining_bzl: CanonicalLabel<'static>,
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    kind: RuleClassKind,
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    exported_name: E,
}

type RuleCallable<'v> = RuleCallableGen<Value<'v>, MutableExportName>;
type FrozenRuleCallable = RuleCallableGen<FrozenValue, FrozenExportName>;

starlark_complex_values!(RuleCallable);
register_avalue_simple_frozen!(FrozenRuleCallable);
register_ty_starlark_value!(RuleCallable<'_>);

impl<V, E: ExportName> fmt::Display for RuleCallableGen<V, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<rule {}>",
            self.exported_name.get().unwrap_or("<unexported>")
        )
    }
}

impl<'v> Freeze for RuleCallable<'v> {
    type Frozen = FrozenRuleCallable;

    fn freeze(self, freezer: &Freezer) -> FreezeResult<Self::Frozen> {
        Ok(FrozenRuleCallable {
            implementation: self.implementation.freeze(freezer)?,
            attributes: self.attributes,
            defining_bzl: self.defining_bzl,
            kind: self.kind,
            exported_name: FrozenExportName {
                value: self.exported_name.value.into_inner(),
            },
        })
    }
}

#[starlark_value(type = FUNCTION_TYPE)]
impl<'v, V, E> StarlarkValue<'v> for RuleCallableGen<V, E>
where
    V: ValueLike<'v>,
    E: ExportName + ProvidesStaticType<'v>,
    Self: ProvidesStaticType<'v>,
{
    type Canonical = RuleCallable<'v>;

    fn name_for_call_stack(&self, _me: Value<'v>) -> String {
        self.exported_name
            .get()
            .unwrap_or("<unexported rule>")
            .to_owned()
    }

    fn export_as(
        &self,
        variable_name: &str,
        _eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<()> {
        if self.kind == RuleClassKind::Test && !variable_name.ends_with("_test") {
            return Err(starlark_error(format!(
                "test rule `{variable_name}` must have a name ending in `_test`"
            )));
        }
        self.exported_name.export(variable_name)
    }

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let exported_name = self
            .exported_name
            .get()
            .ok_or_else(|| starlark_error("an unexported rule cannot be invoked"))?;
        if self.kind == RuleClassKind::Test && !exported_name.ends_with("_test") {
            return Err(starlark_error(format!(
                "test rule `{exported_name}` must have a name ending in `_test`"
            )));
        }
        args.no_positional_args(eval.heap())?;
        let extra = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<crate::starlark::globals::build::BuildExtra>())
            .ok_or_else(|| {
                starlark_error("rules can only be instantiated from BUILD evaluation")
            })?;
        let mut explicit = BTreeMap::new();
        for (name, value) in args.names_map()? {
            let name = name.as_str();
            let schema = self
                .attributes
                .iter()
                .find(|schema| schema.name == name)
                .ok_or_else(|| starlark_error(format!("unknown attribute `{name}`")))?;
            explicit.insert(name, convert_attribute(schema, value, &extra.resolver)?);
        }
        let id = RuleClassId::Starlark {
            defining_bzl: self.defining_bzl.clone(),
            exported_name: exported_name.to_owned(),
        };
        extra
            .builder
            .borrow_mut()
            .instantiate_rule(
                RuleClassSpec::Starlark {
                    id,
                    attributes: &self.attributes,
                    kind: self.kind,
                },
                explicit,
            )
            .map_err(starlark_error)?;
        Ok(Value::new_none())
    }
}

#[starlark_module]
pub(crate) fn bzl_rule_globals(builder: &mut GlobalsBuilder) {
    fn rule<'v>(
        implementation: Value<'v>,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<RuleCallable<'v>> {
        let extra = BzlExtra::from_eval(eval)?;
        let any = Ty::any();
        implementation.check_callable_with(
            [&any],
            std::iter::empty::<(&str, &Ty)>(),
            None,
            None,
            &any,
        )?;
        let mut custom_attributes = Vec::new();
        let mut test = false;
        let mut executable = false;

        for (name, value) in kwargs {
            match name {
                "attrs" => {
                    let attrs = DictRef::from_value(value)
                        .ok_or_else(|| starlark_error("rule(attrs=...) must be a dict"))?;
                    for (name, descriptor) in attrs.iter() {
                        let name = name.unpack_str().ok_or_else(|| {
                            starlark_error("rule attribute names must be strings")
                        })?;
                        let descriptor =
                            AttrDescriptor::from_value(descriptor).ok_or_else(|| {
                                starlark_error(format!(
                                    "attribute `{name}` is not an attr descriptor"
                                ))
                            })?;
                        let mut schema = descriptor.schema.clone();
                        schema.name = name.to_owned();
                        schema.public = !name.starts_with('_');
                        if !schema.public && schema.default.is_none() {
                            return Err(starlark_error(format!(
                                "private attribute `{name}` must have a default"
                            )));
                        }
                        custom_attributes.push(schema);
                    }
                }
                "test" => {
                    test = value
                        .unpack_bool()
                        .ok_or_else(|| starlark_error("rule(test=...) must be a bool"))?;
                }
                "executable" => {
                    executable = value
                        .unpack_bool()
                        .ok_or_else(|| starlark_error("rule(executable=...) must be a bool"))?;
                }
                "doc" => {
                    if !value.is_none() && value.unpack_str().is_none() {
                        return Err(starlark_error("rule(doc=...) must be a string or None"));
                    }
                }
                "cfg" | "outputs" | "implicit_outputs" | "build_setting" | "initializer"
                | "parent" => require_none(name, value)?,
                "provides"
                | "advertised_providers"
                | "fragments"
                | "host_fragments"
                | "toolchains"
                | "exec_compatible_with"
                | "subrules" => require_empty_list(name, value)?,
                "analysis_test" | "dependency_resolution_rule" => require_false(name, value)?,
                _ => {
                    return Err(starlark_error(format!(
                        "unsupported rule parameter `{name}`"
                    )));
                }
            }
        }

        let kind = if test {
            RuleClassKind::Test
        } else if executable {
            RuleClassKind::Executable
        } else {
            RuleClassKind::Ordinary
        };
        let mut attributes = base_rule_attributes(kind);
        for schema in custom_attributes {
            if attributes.iter().any(|base| base.name == schema.name) {
                return Err(starlark_error(format!(
                    "attribute `{}` conflicts with a built-in rule attribute",
                    schema.name
                )));
            }
            attributes.push(schema);
        }
        Ok(RuleCallableGen {
            implementation,
            attributes,
            defining_bzl: extra.resolver.context().clone().into_owned(),
            kind,
            exported_name: MutableExportName {
                value: OnceCell::new(),
            },
        })
    }
}

pub(crate) fn convert_attribute(
    schema: &AttributeSchema,
    value: Value<'_>,
    resolver: &LabelResolver,
) -> starlark::Result<AttributeValue> {
    let terms = if let Some(selector) = SelectorExpression::from_value(value) {
        selector.terms.clone()
    } else {
        vec![SelectorTerm::Direct(RawValue::from_value(value)?)]
    };
    if terms.len() > 1
        && !matches!(
            schema.attribute_type,
            AttributeType::IntList
                | AttributeType::String
                | AttributeType::StringList
                | AttributeType::LabelList
        )
    {
        return Err(starlark_error(format!(
            "attribute `{}` does not support concatenated select() expressions",
            schema.name
        )));
    }
    if !schema.configurable
        && terms
            .iter()
            .any(|term| matches!(term, SelectorTerm::Select(_)))
    {
        return Err(starlark_error(format!(
            "attribute `{}` is not configurable",
            schema.name
        )));
    }

    let mut parts = Vec::new();
    for term in terms {
        match term {
            SelectorTerm::Direct(value) => parts.push(AttributeValuePart::Direct(convert_direct(
                schema.attribute_type,
                value,
                resolver,
            )?)),
            SelectorTerm::Select(selector) => {
                let entries = selector
                    .conditions
                    .into_iter()
                    .map(|(condition, value)| {
                        Ok((
                            condition,
                            value
                                .map(|value| convert_direct(schema.attribute_type, value, resolver))
                                .transpose()?,
                        ))
                    })
                    .collect::<starlark::Result<BTreeMap<_, _>>>()?;
                let default = selector
                    .default
                    .map(|value| {
                        value
                            .map(|value| convert_direct(schema.attribute_type, value, resolver))
                            .transpose()
                    })
                    .transpose()?;
                parts.push(AttributeValuePart::Selector(Selector {
                    entries,
                    default,
                    no_match_error: selector.no_match_error,
                }));
            }
        }
    }

    let value = AttributeValue { parts };
    validate_allow_empty(schema, &value)?;
    Ok(value)
}

fn validate_allow_empty(schema: &AttributeSchema, value: &AttributeValue) -> starlark::Result<()> {
    if schema.allow_empty != Some(false) {
        return Ok(());
    }
    let validate = |value: &DirectAttributeValue| {
        if direct_value_is_empty(value) {
            Err(starlark_error(format!(
                "attribute `{}` may not be empty",
                schema.name
            )))
        } else {
            Ok(())
        }
    };
    for part in &value.parts {
        match part {
            AttributeValuePart::Direct(value) => validate(value)?,
            AttributeValuePart::Selector(selector) => {
                for branch in selector.entries.values() {
                    match branch {
                        Some(value) => validate(value)?,
                        None => {
                            if let Some(default) = &schema.default {
                                validate(default)?;
                            }
                        }
                    }
                }
                if let Some(branch) = &selector.default {
                    match branch {
                        Some(value) => validate(value)?,
                        None => {
                            if let Some(default) = &schema.default {
                                validate(default)?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn direct_value_is_empty(value: &DirectAttributeValue) -> bool {
    match value {
        DirectAttributeValue::String(value) => value.is_empty(),
        DirectAttributeValue::IntList(value) => value.is_empty(),
        DirectAttributeValue::StringList(value) => value.is_empty(),
        DirectAttributeValue::StringListDict(value) => value.is_empty(),
        DirectAttributeValue::StringDict(value) => value.is_empty(),
        DirectAttributeValue::LabelList(value) | DirectAttributeValue::OutputList(value) => {
            value.is_empty()
        }
        DirectAttributeValue::LabelKeyedStringDict(value) => value.is_empty(),
        DirectAttributeValue::LabelListDict(value) => value.is_empty(),
        DirectAttributeValue::StringKeyedLabelDict(value) => value.is_empty(),
        _ => false,
    }
}

fn convert_direct(
    attribute_type: AttributeType,
    value: RawValue,
    resolver: &LabelResolver,
) -> starlark::Result<DirectAttributeValue> {
    let mismatch = |value: &RawValue| {
        starlark_error(format!(
            "expected {attribute_type:?}, got loading value {value:?}"
        ))
    };
    match (attribute_type, value) {
        (AttributeType::Bool, RawValue::Bool(value)) => Ok(DirectAttributeValue::Bool(value)),
        (AttributeType::Int, RawValue::Int(value)) => Ok(DirectAttributeValue::Int(value)),
        (AttributeType::IntList, RawValue::List(values)) => values
            .into_iter()
            .map(|value| match value {
                RawValue::Int(value) => Ok(value),
                other => Err(mismatch(&other)),
            })
            .collect::<starlark::Result<Vec<_>>>()
            .map(DirectAttributeValue::IntList),
        (AttributeType::String, RawValue::String(value)) => Ok(DirectAttributeValue::String(value)),
        (AttributeType::StringList, RawValue::List(values)) => values
            .into_iter()
            .map(|value| match value {
                RawValue::String(value) => Ok(value),
                other => Err(mismatch(&other)),
            })
            .collect::<starlark::Result<Vec<_>>>()
            .map(DirectAttributeValue::StringList),
        (AttributeType::StringListDict, RawValue::Dict(values)) => values
            .into_iter()
            .map(|(key, value)| {
                let RawValue::String(key) = key else {
                    return Err(starlark_error("string_list_dict keys must be strings"));
                };
                let RawValue::List(values) = value else {
                    return Err(starlark_error("string_list_dict values must be lists"));
                };
                let values = values
                    .into_iter()
                    .map(|value| match value {
                        RawValue::String(value) => Ok(value),
                        other => Err(mismatch(&other)),
                    })
                    .collect::<starlark::Result<Vec<_>>>()?;
                Ok((key, values))
            })
            .collect::<starlark::Result<BTreeMap<_, _>>>()
            .map(DirectAttributeValue::StringListDict),
        (AttributeType::StringDict, RawValue::Dict(values)) => values
            .into_iter()
            .map(|(key, value)| match (key, value) {
                (RawValue::String(key), RawValue::String(value)) => Ok((key, value)),
                (key, value) => Err(starlark_error(format!(
                    "expected string_dict entry, got {key:?}: {value:?}"
                ))),
            })
            .collect::<starlark::Result<BTreeMap<_, _>>>()
            .map(DirectAttributeValue::StringDict),
        (AttributeType::Label, RawValue::None) => Ok(DirectAttributeValue::None),
        (AttributeType::Label, value) => {
            label_from_raw(value, resolver).map(DirectAttributeValue::Label)
        }
        (AttributeType::LabelList, RawValue::List(values)) => values
            .into_iter()
            .map(|value| label_from_raw(value, resolver))
            .collect::<starlark::Result<Vec<_>>>()
            .map(DirectAttributeValue::LabelList),
        (AttributeType::LabelKeyedStringDict, RawValue::Dict(values)) => {
            let mut converted = BTreeMap::new();
            for (key, value) in values {
                let key = label_from_raw(key, resolver)?;
                let RawValue::String(value) = value else {
                    return Err(starlark_error(
                        "label_keyed_string_dict values must be strings",
                    ));
                };
                if converted.insert(key.clone(), value).is_some() {
                    return Err(starlark_error(format!(
                        "duplicate label_keyed_string_dict key `{key}`"
                    )));
                }
            }
            Ok(DirectAttributeValue::LabelKeyedStringDict(converted))
        }
        (AttributeType::LabelListDict, RawValue::Dict(values)) => values
            .into_iter()
            .map(|(key, value)| {
                let RawValue::String(key) = key else {
                    return Err(starlark_error("label_list_dict keys must be strings"));
                };
                let RawValue::List(values) = value else {
                    return Err(starlark_error("label_list_dict values must be lists"));
                };
                let values = values
                    .into_iter()
                    .map(|value| label_from_raw(value, resolver))
                    .collect::<starlark::Result<Vec<_>>>()?;
                Ok((key, values))
            })
            .collect::<starlark::Result<BTreeMap<_, _>>>()
            .map(DirectAttributeValue::LabelListDict),
        (AttributeType::StringKeyedLabelDict, RawValue::Dict(values)) => values
            .into_iter()
            .map(|(key, value)| {
                let RawValue::String(key) = key else {
                    return Err(starlark_error(
                        "string_keyed_label_dict keys must be strings",
                    ));
                };
                Ok((key, label_from_raw(value, resolver)?))
            })
            .collect::<starlark::Result<BTreeMap<_, _>>>()
            .map(DirectAttributeValue::StringKeyedLabelDict),
        (AttributeType::Output, value) => {
            label_from_raw(value, resolver).map(DirectAttributeValue::Output)
        }
        (AttributeType::OutputList, RawValue::List(values)) => values
            .into_iter()
            .map(|value| label_from_raw(value, resolver))
            .collect::<starlark::Result<Vec<_>>>()
            .map(DirectAttributeValue::OutputList),
        (_, value) => Err(mismatch(&value)),
    }
}

fn label_from_raw(
    value: RawValue,
    resolver: &LabelResolver,
) -> starlark::Result<CanonicalLabel<'static>> {
    match value {
        RawValue::String(value) => resolver.resolve(&value).map_err(starlark_error),
        RawValue::Label(value) => Ok(value),
        value => Err(starlark_error(format!(
            "expected a string or Label, got {value:?}"
        ))),
    }
}

pub(crate) fn resolve_starlark_rule_class(
    module: &FrozenModule,
    id: &RuleClassId,
) -> anyhow::Result<OwnedFrozenValue> {
    let RuleClassId::Starlark {
        defining_bzl,
        exported_name,
    } = id
    else {
        anyhow::bail!("expected a Starlark rule class ID");
    };
    let owned = module.get(exported_name)?;
    let callable = owned
        .value()
        .downcast_ref::<FrozenRuleCallable>()
        .ok_or_else(|| anyhow::anyhow!("exported value `{exported_name}` is not a rule"))?;
    if &callable.defining_bzl != defining_bzl
        || callable.exported_name.value.as_deref() != Some(exported_name)
    {
        anyhow::bail!("resolved rule class does not match {id:?}");
    }
    Ok(owned)
}

fn frozen_rule_class(rule_class: &OwnedFrozenValue) -> &FrozenRuleCallable {
    rule_class
        .value()
        .downcast_ref::<FrozenRuleCallable>()
        .expect("loaded Starlark rule classes retain their frozen callable")
}

pub(crate) fn frozen_rule_class_attributes(rule_class: &OwnedFrozenValue) -> &[AttributeSchema] {
    &frozen_rule_class(rule_class).attributes
}

pub(crate) fn frozen_rule_class_name(rule_class: &OwnedFrozenValue) -> &str {
    frozen_rule_class(rule_class)
        .exported_name
        .value
        .as_deref()
        .expect("loaded Starlark rule classes are exported")
}

pub(crate) fn frozen_rule_class_kind(rule_class: &OwnedFrozenValue) -> RuleClassKind {
    frozen_rule_class(rule_class).kind
}

fn starlark_error(error: impl fmt::Display) -> starlark::Error {
    starlark::Error::new_native(anyhow::anyhow!(error.to_string()))
}
