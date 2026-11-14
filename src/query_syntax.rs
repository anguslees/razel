use chumsky::prelude::*;
use chumsky::span::{SimpleSpan, Spanned};
use std::fmt;

type RawQueryParserError<'src> = Rich<'src, char, SimpleSpan<usize>, String>;
type OwnedQueryParserError = RawQueryParserError<'static>;

#[derive(Debug)]
struct QueryParserError(OwnedQueryParserError);

impl fmt::Display for QueryParserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for QueryParserError {}

#[derive(Debug)]
pub struct QuerySyntaxError {
    errors: Vec<QueryParserError>,
}

impl<'src> From<Vec<RawQueryParserError<'src>>> for QuerySyntaxError {
    fn from(errors: Vec<RawQueryParserError<'src>>) -> Self {
        Self {
            errors: errors
                .into_iter()
                .map(|error| QueryParserError(error.into_owned()))
                .collect(),
        }
    }
}

impl fmt::Display for QuerySyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Failed to parse query: ")?;
        for (index, error) in self.errors.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            fmt::Display::fmt(error, formatter)?;
        }
        formatter.write_str("\nSee https://bazel.build/reference/query for syntax")
    }
}

impl std::error::Error for QuerySyntaxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.errors.first().map(|error| error as _)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr<'a> {
    String(&'a str),
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
                } else if matches!(s, "union" | "intersect" | "except" | "let" | "in") {
                    Err(Rich::custom(span, "Reserved query keyword must be quoted"))
                } else {
                    Ok(s)
                }
            });

        let function = ident
            .then(
                expr.clone()
                    .separated_by(just(',').padded())
                    .collect::<Vec<_>>()
                    .delimited_by(just('(').padded(), just(')').padded()),
            )
            .map(|(name, args)| Expr::Function(name, args));

        let variable = just('$').ignore_then(ident).map(Expr::Variable);
        let let_binding = text::keyword("let")
            .padded()
            .ignore_then(ident)
            .then_ignore(just('=').padded())
            .then(expr.clone())
            .then_ignore(text::keyword("in").padded())
            .then(expr.clone())
            .map(|((name, value), body)| Expr::Let(name, Box::new(value), Box::new(body)));

        let string_literal = just('"')
            .ignore_then(none_of('"').repeated().to_slice())
            .then_ignore(just('"'));
        let single_string_literal = just('\'')
            .ignore_then(none_of('\'').repeated().to_slice())
            .then_ignore(just('\''));
        let quoted_string = string_literal.or(single_string_literal).map(Expr::String);
        let pattern = unquoted_word.map(Expr::String);

        let atom = choice((
            paren_expr,
            function,
            let_binding,
            variable,
            quoted_string,
            pattern,
        ))
        .padded()
        .map_with(|ast, e| Spanned {
            inner: ast,
            span: e.span(),
        });

        let op_intersect = choice((just("^"), text::keyword("intersect")))
            .padded()
            .to(SetOp::Intersect);
        let op_union = choice((just("+"), text::keyword("union")))
            .padded()
            .to(SetOp::Union);
        let op_diff = choice((just("-"), text::keyword("except")))
            .padded()
            .to(SetOp::Difference);
        atom.clone().foldl(
            choice((op_union, op_diff, op_intersect))
                .then(atom)
                .repeated(),
            |left, (op, right)| {
                let span = SimpleSpan::from(left.span.start..right.span.end);
                Spanned {
                    inner: Expr::SetOp(op, Box::new(left), Box::new(right)),
                    span,
                }
            },
        )
    })
}

pub fn parse(query: &str) -> Result<Spanned<Expr<'_>>, QuerySyntaxError> {
    parser()
        .parse(query)
        .into_result()
        .map_err(QuerySyntaxError::from)
}

impl Expr<'_> {
    /// Whether this expression refers to variable `name`, other than inside a `let` that binds
    /// `name` again.
    pub fn references(&self, name: &str) -> bool {
        match self {
            Expr::String(_) => false,
            Expr::Variable(variable) => *variable == name,
            Expr::Function(_, args) => args.iter().any(|argument| argument.inner.references(name)),
            Expr::SetOp(_, left, right) => {
                left.inner.references(name) || right.inner.references(name)
            }
            Expr::Let(variable, value, body) => {
                value.inner.references(name) || (*variable != name && body.inner.references(name))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn query_syntax_error_preserves_first_parser_error() {
        let error = parse("(").unwrap_err();

        assert!(error.to_string().starts_with("Failed to parse query: "));
        assert!(error.source().is_some());
    }

    fn parse_expr(input: &str) -> Expr<'_> {
        parse(input).expect("Parse failed").inner
    }

    #[test]
    fn wildcard() {
        assert_eq!(parse_expr("//foo:bar"), Expr::String("//foo:bar"));
        assert_eq!(parse_expr("//foo/..."), Expr::String("//foo/..."));
        assert_eq!(parse_expr("//foo/...:*"), Expr::String("//foo/...:*"));
        assert_eq!(parse_expr("//foo:*"), Expr::String("//foo:*"));
        assert_eq!(parse_expr("//foo:all"), Expr::String("//foo:all"));
    }

    #[test]
    fn deps_function() {
        let Expr::Function(name, args) = parse_expr("deps(//foo)") else {
            panic!("Expected deps function");
        };
        assert_eq!(name, "deps");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].inner, Expr::String("//foo"));
    }

    #[test]
    fn precedence() {
        let Expr::SetOp(SetOp::Intersect, left, right) = parse_expr("//a + //b ^ //c") else {
            panic!("Expected outer intersection");
        };
        assert!(matches!(left.inner, Expr::SetOp(SetOp::Union, _, _)));
        assert_eq!(right.inner, Expr::String("//c"));
    }

    #[test]
    fn integer_arguments_and_top_level_words() {
        let Expr::Function(_, args) = parse_expr("deps(//foo, 7)") else {
            panic!("Expected deps function");
        };
        assert_eq!(args[1].inner, Expr::String("7"));
        assert_eq!(parse_expr("7"), Expr::String("7"));
    }

    #[test]
    fn unquoted_word_rules() {
        assert!(parser().parse("-foo").into_result().is_err());
        assert!(parser().parse("*foo").into_result().is_err());
        assert!(parser().parse("foo+bar").into_result().is_err());
        assert_eq!(parse_expr("@@foo+bar"), Expr::String("@@foo+bar"));
        assert!(parser().parse("union").into_result().is_err());
        assert_eq!(parse_expr("'union'"), Expr::String("union"));
    }

    #[test]
    fn references_follow_let_scoping() {
        assert!(parse_expr("$x").references("x"));
        assert!(!parse_expr("$x").references("y"));
        assert!(parse_expr("deps(//:a + $x, 1)").references("x"));
        // A nested `let` of the same name hides the outer variable in its body...
        assert!(!parse_expr("let x = //:a in $x").references("x"));
        // ...but not in its value, which is evaluated in the outer scope.
        assert!(parse_expr("let x = $x in //:a").references("x"));
        assert!(parse_expr("let y = //:a in $x ^ $y").references("x"));
    }
}
