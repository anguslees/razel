use crate::bazel::Configuration;
use crate::bazel::label::{Label, MAIN_REPO_ROOT, Repo, parse_label};
use crate::stream_tee::{StreamTee, StreamTeeExt};
use crate::workspace::Workspace;
use chumsky::prelude::*;
use chumsky::span::{SimpleSpan, Spanned};
use futures::stream::{self, BoxStream, StreamExt};
use std::collections::HashMap;
use std::marker::Unpin;
use std::sync::Arc;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;

pub type QueryResult<'a> = Result<Label<'a, Repo<'a>>, String>;
pub type QueryStream<'a> = BoxStream<'a, QueryResult<'a>>;

#[derive(Clone, Default)]
pub struct QueryContext<'a> {
    pub variables: HashMap<&'a str, StreamTee<QueryStream<'a>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr<'a> {
    Target(&'a str),
    String(&'a str),
    Int(i64),
    Function(&'a str, Vec<Spanned<Expr<'a>>>),
    SetOp(SetOp, Box<Spanned<Expr<'a>>>, Box<Spanned<Expr<'a>>>),
    Let(&'a str, Box<Spanned<Expr<'a>>>, Box<Spanned<Expr<'a>>>),
    Variable(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    Union,
    Difference,
    Intersect,
}

#[allow(clippy::let_and_return)]
pub fn parser<'a>() -> impl Parser<'a, &'a str, Spanned<Expr<'a>>, extra::Err<Rich<'a, char>>> {
    recursive(|expr| {
        let paren_expr = expr
            .clone()
            .delimited_by(just('(').padded(), just(')').padded())
            .map(|s: Spanned<Expr<'a>>| s.inner);

        let ident = text::ident().to_slice();
        // Unquoted words: alphanumeric and */@.-_:$~[]
        // Unquoted words may not start with - or *
        // Unquoted words may not contain + unless it starts with @@
        let unquoted_word = any::<&str, extra::Err<Rich<'a, char>>>()
            .filter(|c: &char| c.is_ascii_alphanumeric() || "*/@.-_:$~[]+".contains(*c))
            .repeated()
            .at_least(1)
            .to_slice()
            .try_map(|s: &str, span| {
                if s.starts_with('-') || s.starts_with('*') {
                    Err(Rich::custom(
                        span,
                        "Unquoted word cannot start with '-' or '*'",
                    ))
                } else if s.contains('+') && !s.starts_with("@@") {
                    Err(Rich::custom(
                        span,
                        "Unquoted word cannot contain '+' unless it starts with '@@'",
                    ))
                } else {
                    Ok(s)
                }
            });

        // Functions: e.g. deps(//foo), kind("*.cc", deps(//bar))
        let int_arg = unquoted_word
            .try_map(|s: &str, span| {
                s.parse::<i64>()
                    .map(Expr::Int)
                    .map_err(|e| Rich::custom(span, format!("Invalid integer: {}", e)))
            })
            .map_with(|ast, e| Spanned {
                inner: ast,
                span: e.span(),
            });

        let function = ident
            .then(
                choice((int_arg, expr.clone()))
                    .separated_by(just(',').padded())
                    .collect::<Vec<_>>()
                    .delimited_by(just('(').padded(), just(')').padded()),
            )
            .map(|(name, args)| Expr::Function(name, args));

        // Variable ref: e.g. $foo
        let variable = just('$').ignore_then(ident).map(Expr::Variable);

        // Let binding: let name = expr in expr
        let let_binding = text::keyword("let")
            .padded()
            .ignore_then(ident)
            .then_ignore(just('=').padded())
            .then(expr.clone())
            .then_ignore(text::keyword("in").padded())
            .then(expr.clone())
            .map(|((name, val), body)| Expr::Let(name, Box::new(val), Box::new(body)));

        // Strings
        let string_literal = just('"')
            .ignore_then(none_of('"').repeated().to_slice())
            .then_ignore(just('"'));

        let single_string_literal = just('\'')
            .ignore_then(none_of('\'').repeated().to_slice())
            .then_ignore(just('\''));

        let quoted_string = string_literal.or(single_string_literal).map(Expr::String);

        let target = unquoted_word.map(Expr::Target);

        let atom = choice((
            paren_expr,
            function,
            let_binding,
            variable,
            quoted_string,
            target,
        ))
        .padded()
        .map_with(|ast, e| Spanned {
            inner: ast,
            span: e.span(),
        });

        // Intersect operator has higher precedence
        let op_intersect = choice((just("^"), text::keyword("intersect")))
            .padded()
            .to(SetOp::Intersect);

        let intersection =
            atom.clone()
                .foldl(op_intersect.then(atom).repeated(), |left, (op, right)| {
                    let span = SimpleSpan::from(left.span.start..right.span.end);
                    Spanned {
                        inner: Expr::SetOp(op, Box::new(left), Box::new(right)),
                        span,
                    }
                });

        // Union / Difference have lower precedence
        let op_union = choice((just("+"), text::keyword("union")))
            .padded()
            .to(SetOp::Union);

        let op_diff = choice((just("-"), text::keyword("except")))
            .padded()
            .to(SetOp::Difference);

        let set_op = intersection.clone().foldl(
            choice((op_union, op_diff)).then(intersection).repeated(),
            |left, (op, right)| {
                let span = SimpleSpan::from(left.span.start..right.span.end);
                Spanned {
                    inner: Expr::SetOp(op, Box::new(left), Box::new(right)),
                    span,
                }
            },
        );

        set_op
    })
}

impl<'a> Expr<'a> {
    pub fn eval(&self, ctx: &QueryContext<'a>) -> QueryStream<'a> {
        match self {
            Expr::Target(s) | Expr::String(s) => match parse_label(s, &MAIN_REPO_ROOT) {
                Ok(label) => stream::once(async move { Ok(label) }).boxed(),
                Err(e) => stream::once(async move { Err(e.to_string()) }).boxed(),
            },
            Expr::Int(_) => {
                stream::once(async { Err("Int not supported out of function context".to_string()) })
                    .boxed()
            }
            Expr::Function(name, _args) => {
                // Return an empty or unimplemented error stream for now
                let err_msg = format!("Function {} not fully implemented", name);
                stream::once(async move { Err(err_msg) }).boxed()
            }
            Expr::Let(name, val, body) => {
                // Evaluate the let value stream
                let val_stream = val.inner.eval(ctx);
                // Tee it so multiple $refs can consume it
                let val_tee = val_stream.tee();

                let mut new_ctx = ctx.clone();
                new_ctx.variables.insert(name, val_tee);

                body.inner.eval(&new_ctx)
            }
            Expr::Variable(name) => {
                if let Some(tee) = ctx.variables.get(name) {
                    // clone the tee giving us a fresh consumer of the items, then box it
                    tee.clone().boxed()
                } else {
                    let err_msg = format!("Undefined variable {}", name);
                    stream::once(async move { Err(err_msg) }).boxed()
                }
            }
            Expr::SetOp(SetOp::Union, left, right) => {
                let l_stream = left.inner.eval(ctx);
                let r_stream = right.inner.eval(ctx);

                // Track seen labels
                let seen =
                    std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
                let seen_for_l = seen.clone();

                let l_mapped = l_stream.map(move |res| {
                    if let Ok(l) = &res {
                        let mut s = seen_for_l.lock().expect("Mutex poisoned");
                        s.insert(l.clone());
                    }
                    res
                });

                let r_filtered = r_stream.filter(move |res| match res {
                    Ok(l) => {
                        let s = seen.lock().expect("Mutex poisoned");
                        futures::future::ready(!s.contains(l))
                    }
                    Err(_) => futures::future::ready(true),
                });

                l_mapped.chain(r_filtered).boxed()
            }
            Expr::SetOp(SetOp::Intersect, _, _) => {
                // Intersect requires knowing contents of right.
                stream::once(async move { Err("Intersect not yet implemented".to_string()) })
                    .boxed()
            }
            Expr::SetOp(SetOp::Difference, _, _) => {
                stream::once(async move { Err("Difference not yet implemented".to_string()) })
                    .boxed()
            }
        }
    }
}

pub async fn query<W>(out: &mut W, _config: Arc<Configuration>, query: &str) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let workspace = Workspace::new(".").await?;
    let module = workspace.main_module().await?;

    // Construct repos from bzlmod declarations
    // Global Map of Canonical name -> FusedFuture<dyn Repo>
    // Each repo (including _main) needs a Map of repo name -> Canonical name

    let ast = parser().parse(query).into_result().map_err(|errs| {
        anyhow::anyhow!(
            "Failed to parse query: {}\nSee https://bazel.build/reference/query for syntax",
            errs.into_iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;

    // Evaluate the query!
    let mut result_stream = ast.inner.eval(&QueryContext::default());

    while let Some(res) = result_stream.next().await {
        match res {
            Ok(label) => {
                out.write_all(format!("{}\n", label).as_bytes()).await?;
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Query evaluation error: {}", e));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Expr<'_> {
        parser()
            .parse(input)
            .into_result()
            .expect("Parse failed")
            .inner
    }

    #[test]
    fn test_target() {
        assert_eq!(parse("//foo:bar"), Expr::Target("//foo:bar"));
    }

    #[test]
    fn test_deps_function() {
        if let Expr::Function(name, args) = parse("deps(//foo)") {
            assert_eq!(name, "deps");
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].inner, Expr::Target("//foo"));
        } else {
            panic!("Expected deps function");
        }
    }

    #[test]
    fn test_union() {
        match parse("//foo + //bar") {
            Expr::SetOp(SetOp::Union, left, right) => {
                assert_eq!(left.inner, Expr::Target("//foo"));
                assert_eq!(right.inner, Expr::Target("//bar"));
            }
            _ => panic!("Expected union op"),
        }
    }

    #[test]
    fn test_intersection() {
        match parse("//foo ^ //bar") {
            Expr::SetOp(SetOp::Intersect, left, right) => {
                assert_eq!(left.inner, Expr::Target("//foo"));
                assert_eq!(right.inner, Expr::Target("//bar"));
            }
            _ => panic!("Expected intersect op"),
        }
    }

    #[test]
    fn test_precedence() {
        match parse("//a + //b ^ //c") {
            Expr::SetOp(SetOp::Union, left, right) => {
                assert_eq!(left.inner, Expr::Target("//a"));
                match &right.inner {
                    Expr::SetOp(SetOp::Intersect, inner_left, inner_right) => {
                        assert_eq!(inner_left.inner, Expr::Target("//b"));
                        assert_eq!(inner_right.inner, Expr::Target("//c"));
                    }
                    _ => panic!("Expected inner intersect"),
                }
            }
            _ => panic!("Expected outer union"),
        }
    }
    #[test]
    fn test_int_argument_in_function() {
        if let Expr::Function(name, args) = parse("deps(//foo, 7)") {
            assert_eq!(name, "deps");
            assert_eq!(args.len(), 2);
            assert_eq!(args[0].inner, Expr::Target("//foo"));
            assert_eq!(args[1].inner, Expr::Int(7));
        } else {
            panic!("Expected deps function with Int argument");
        }
    }

    #[test]
    fn test_pure_int_parses_as_target() {
        // Pure integers are not valid top-level expressions in Bazel queries.
        // Outside a function arg they are parsed as targets.
        assert_eq!(
            parser()
                .parse("7")
                .into_result()
                .expect("parse failed")
                .inner,
            Expr::Target("7")
        );
    }

    #[test]
    fn test_unquoted_word_rules() {
        // May not start with - or *
        assert!(parser().parse("-foo").into_result().is_err());
        assert!(parser().parse("*foo").into_result().is_err());

        // Cannot contain +
        assert!(parser().parse("foo+bar").into_result().is_err());

        // May contain + if starts with @@
        assert_eq!(
            parser()
                .parse("@@foo+bar")
                .into_result()
                .expect("parse failed")
                .inner,
            Expr::Target("@@foo+bar")
        );
    }
}
