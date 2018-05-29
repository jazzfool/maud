use proc_macro::{
    Delimiter,
    Literal,
    Spacing,
    Span,
    TokenStream,
    TokenTree,
};
use std::mem;

use literalext::LiteralExt;

use ast;
use ParseResult;

pub fn parse(input: TokenStream) -> ParseResult<Vec<ast::Markup>> {
    Parser::new(input).markups()
}

#[derive(Clone)]
struct Parser {
    /// Indicates whether we're inside an attribute node.
    in_attr: bool,
    input: <TokenStream as IntoIterator>::IntoIter,
}

impl Iterator for Parser {
    type Item = TokenTree;

    fn next(&mut self) -> Option<TokenTree> {
        self.input.next()
    }
}

impl Parser {
    fn new(input: TokenStream) -> Parser {
        Parser {
            in_attr: false,
            input: input.into_iter(),
        }
    }

    fn with_input(&self, input: TokenStream) -> Parser {
        Parser {
            in_attr: self.in_attr,
            input: input.into_iter(),
        }
    }

    /// Returns the next token in the stream without consuming it.
    fn peek(&mut self) -> Option<TokenTree> {
        self.clone().next()
    }

    /// Returns the next two tokens in the stream without consuming them.
    fn peek2(&mut self) -> Option<(TokenTree, Option<TokenTree>)> {
        let mut clone = self.clone();
        clone.next().map(|first| (first, clone.next()))
    }

    /// Advances the cursor by one step.
    fn advance(&mut self) {
        self.next();
    }

    /// Advances the cursor by two steps.
    fn advance2(&mut self) {
        self.next();
        self.next();
    }

    /// Overwrites the current parser state with the given parameter.
    fn commit(&mut self, attempt: Parser) {
        *self = attempt;
    }

    /// Returns an `Err` with the given message.
    fn error<T, E: Into<String>>(&self, message: E) -> ParseResult<T> {
        Err(message.into())
    }

    /// Parses and renders multiple blocks of markup.
    fn markups(&mut self) -> ParseResult<Vec<ast::Markup>> {
        let mut result = Vec::new();
        loop {
            match self.peek2() {
                None => break,
                Some((TokenTree::Punct(ref punct), _)) if punct.as_char() == ';' => self.advance(),
                Some((
                    TokenTree::Punct(ref punct),
                    Some(TokenTree::Ident(ref ident)),
                )) if punct.as_char() == '@' && ident.to_string() == "let" => {
                    self.advance2();
                    let keyword = TokenTree::Ident(ident.clone());
                    result.push(self.let_expr(keyword)?);
                },
                _ => result.push(self.markup()?),
            }
        }
        Ok(result)
    }

    /// Parses and renders a single block of markup.
    fn markup(&mut self) -> ParseResult<ast::Markup> {
        let token = match self.peek() {
            Some(token) => token,
            None => return self.error("unexpected end of input"),
        };
        let markup = match token {
            // Literal
            TokenTree::Literal(lit) => {
                self.advance();
                self.literal(&lit)?
            },
            // Special form
            TokenTree::Punct(ref punct) if punct.as_char() == '@' => {
                self.advance();
                match self.next() {
                    Some(TokenTree::Ident(ident)) => {
                        let keyword = TokenTree::Ident(ident.clone());
                        match ident.to_string().as_str() {
                            "if" => {
                                let mut segments = Vec::new();
                                self.if_expr(vec![keyword], &mut segments)?;
                                ast::Markup::Special { segments }
                            },
                            "while" => self.while_expr(keyword)?,
                            "for" => self.for_expr(keyword)?,
                            "match" => self.match_expr(keyword)?,
                            "let" => return self.error("@let only works inside a block"),
                            other => return self.error(format!("unknown keyword `@{}`", other)),
                        }
                    },
                    _ => return self.error("expected keyword after `@`"),
                }
            },
            // Element
            TokenTree::Ident(_) => {
                let name = self.namespaced_name()?;
                self.element(name)?
            },
            // Splice
            TokenTree::Group(ref group) if group.delimiter() == Delimiter::Parenthesis => {
                self.advance();
                ast::Markup::Splice { expr: group.stream() }
            }
            // Block
            TokenTree::Group(ref group) if group.delimiter() == Delimiter::Brace => {
                self.advance();
                ast::Markup::Block(self.block(group.stream(), group.span())?)
            },
            // ???
            _ => return self.error("invalid syntax"),
        };
        Ok(markup)
    }

    /// Parses and renders a literal string.
    fn literal(&mut self, lit: &Literal) -> ParseResult<ast::Markup> {
        if let Some(s) = lit.parse_string() {
            Ok(ast::Markup::Literal {
                content: s.to_string(),
                span: lit.span(),
            })
        } else {
            self.error("expected string")
        }
    }

    /// Parses an `@if` expression.
    ///
    /// The leading `@if` should already be consumed.
    fn if_expr(
        &mut self,
        prefix: Vec<TokenTree>,
        segments: &mut Vec<ast::Special>,
    ) -> ParseResult<()> {
        let mut head = prefix;
        let body = loop {
            match self.next() {
                Some(TokenTree::Group(ref block)) if block.delimiter() == Delimiter::Brace => {
                    break self.block(block.stream(), block.span())?;
                },
                Some(token) => head.push(token),
                None => return self.error("unexpected end of @if expression"),
            }
        };
        segments.push(ast::Special { head: head.into_iter().collect(), body });
        self.else_if_expr(segments)
    }

    /// Parses an optional `@else if` or `@else`.
    ///
    /// The leading `@else if` or `@else` should *not* already be consumed.
    fn else_if_expr(&mut self, segments: &mut Vec<ast::Special>) -> ParseResult<()> {
        match self.peek2() {
            Some((
                TokenTree::Punct(ref punct),
                Some(TokenTree::Ident(ref else_keyword)),
            )) if punct.as_char() == '@' && else_keyword.to_string() == "else" => {
                self.advance2();
                let else_keyword = TokenTree::Ident(else_keyword.clone());
                match self.peek() {
                    // `@else if`
                    Some(TokenTree::Ident(ref if_keyword)) if if_keyword.to_string() == "if" => {
                        self.advance();
                        let if_keyword = TokenTree::Ident(if_keyword.clone());
                        self.if_expr(vec![else_keyword, if_keyword], segments)
                    },
                    // Just an `@else`
                    _ => {
                        match self.next() {
                            Some(TokenTree::Group(ref group)) if group.delimiter() == Delimiter::Brace => {
                                let body = self.block(group.stream(), group.span())?;
                                segments.push(ast::Special {
                                    head: vec![else_keyword].into_iter().collect(),
                                    body,
                                });
                                Ok(())
                            },
                            _ => self.error("expected body for @else"),
                        }
                    },
                }
            },
            // We didn't find an `@else`; stop
            _ => Ok(()),
        }
    }

    /// Parses and renders an `@while` expression.
    ///
    /// The leading `@while` should already be consumed.
    fn while_expr(&mut self, keyword: TokenTree) -> ParseResult<ast::Markup> {
        let mut head = vec![keyword];
        let body = loop {
            match self.next() {
                Some(TokenTree::Group(ref block)) if block.delimiter() == Delimiter::Brace => {
                    break self.block(block.stream(), block.span())?;
                },
                Some(token) => head.push(token),
                None => return self.error("unexpected end of @while expression"),
            }
        };
        Ok(ast::Markup::Special {
            segments: vec![ast::Special { head: head.into_iter().collect(), body }],
        })
    }

    /// Parses a `@for` expression.
    ///
    /// The leading `@for` should already be consumed.
    fn for_expr(&mut self, keyword: TokenTree) -> ParseResult<ast::Markup> {
        let mut head = vec![keyword];
        loop {
            match self.next() {
                Some(TokenTree::Ident(ref in_keyword)) if in_keyword.to_string() == "in" => {
                    head.push(TokenTree::Ident(in_keyword.clone()));
                    break;
                },
                Some(token) => head.push(token),
                None => return self.error("unexpected end of @for expression"),
            }
        }
        let body = loop {
            match self.next() {
                Some(TokenTree::Group(ref block)) if block.delimiter() == Delimiter::Brace => {
                    break self.block(block.stream(), block.span())?;
                },
                Some(token) => head.push(token),
                None => return self.error("unexpected end of @for expression"),
            }
        };
        Ok(ast::Markup::Special {
            segments: vec![ast::Special { head: head.into_iter().collect(), body }],
        })
    }

    /// Parses a `@match` expression.
    ///
    /// The leading `@match` should already be consumed.
    fn match_expr(&mut self, keyword: TokenTree) -> ParseResult<ast::Markup> {
        let mut head = vec![keyword];
        let (arms, arms_span) = loop {
            match self.next() {
                Some(TokenTree::Group(ref body)) if body.delimiter() == Delimiter::Brace => {
                    let span = body.span();
                    break (self.with_input(body.stream()).match_arms()?, span);
                },
                Some(token) => head.push(token),
                None => return self.error("unexpected end of @match expression"),
            }
        };
        Ok(ast::Markup::Match { head: head.into_iter().collect(), arms, arms_span })
    }

    fn match_arms(&mut self) -> ParseResult<Vec<ast::Special>> {
        let mut arms = Vec::new();
        while let Some(arm) = self.match_arm()? {
            arms.push(arm);
        }
        Ok(arms)
    }

    fn match_arm(&mut self) -> ParseResult<Option<ast::Special>> {
        let mut head = Vec::new();
        loop {
            match self.peek2() {
                Some((TokenTree::Punct(ref eq), Some(TokenTree::Punct(ref gt))))
                if eq.as_char() == '=' && gt.as_char() == '>' && eq.spacing() == Spacing::Joint => {
                    self.advance2();
                    head.push(TokenTree::Punct(eq.clone()));
                    head.push(TokenTree::Punct(gt.clone()));
                    break;
                },
                Some((token, _)) => {
                    self.advance();
                    head.push(token);
                },
                None =>
                    if head.is_empty() {
                        return Ok(None);
                    } else {
                        return self.error("unexpected end of @match pattern");
                    },
            }
        }
        let body = match self.next() {
            // $pat => { $stmts }
            Some(TokenTree::Group(ref body)) if body.delimiter() == Delimiter::Brace => {
                let body = self.block(body.stream(), body.span())?;
                // Trailing commas are optional if the match arm is a braced block
                if let Some(TokenTree::Punct(ref punct)) = self.peek() {
                    if punct.as_char() == ',' {
                        self.advance();
                    }
                }
                body
            },
            // $pat => $expr
            Some(first_token) => {
                let mut span = first_token.span();
                let mut body = vec![first_token];
                loop {
                    match self.next() {
                        Some(TokenTree::Punct(ref punct)) if punct.as_char() == ',' => break,
                        Some(token) => {
                            if let Some(bigger_span) = span.join(token.span()) {
                                span = bigger_span;
                            }
                            body.push(token);
                        },
                        None => return self.error("unexpected end of @match arm"),
                    }
                }
                self.block(body.into_iter().collect(), span)?
            },
            None => return self.error("unexpected end of @match arm"),
        };
        Ok(Some(ast::Special { head: head.into_iter().collect(), body }))
    }

    /// Parses a `@let` expression.
    ///
    /// The leading `@let` should already be consumed.
    fn let_expr(&mut self, keyword: TokenTree) -> ParseResult<ast::Markup> {
        let mut tokens = vec![keyword];
        loop {
            match self.next() {
                Some(token) => {
                    match token {
                        TokenTree::Punct(ref punct) if punct.as_char() == '=' => {
                            tokens.push(token.clone());
                            break;
                        },
                        _ => tokens.push(token),
                    }
                },
                None => return self.error("unexpected end of @let expression"),
            }
        }
        loop {
            match self.next() {
                Some(token) => {
                    match token {
                        TokenTree::Punct(ref punct) if punct.as_char() == ';' => {
                            tokens.push(token.clone());
                            break;
                        },
                        _ => tokens.push(token),
                    }
                },
                None => return self.error("unexpected end of @let expression"),
            }
        }
        Ok(ast::Markup::Let { tokens: tokens.into_iter().collect() })
    }

    /// Parses an element node.
    ///
    /// The element name should already be consumed.
    fn element(&mut self, name: TokenStream) -> ParseResult<ast::Markup> {
        if self.in_attr {
            return self.error("unexpected element, you silly bumpkin");
        }
        let attrs = self.attrs()?;
        let body = match self.peek() {
            Some(TokenTree::Punct(ref punct))
            if punct.as_char() == ';' || punct.as_char() == '/' => {
                // Void element
                self.advance();
                None
            },
            _ => Some(Box::new(self.markup()?)),
        };
        Ok(ast::Markup::Element { name, attrs, body })
    }

    /// Parses the attributes of an element.
    fn attrs(&mut self) -> ParseResult<ast::Attrs> {
        let mut classes_static = Vec::new();
        let mut classes_toggled = Vec::new();
        let mut ids = Vec::new();
        let mut attrs = Vec::new();
        loop {
            let mut attempt = self.clone();
            let maybe_name = attempt.namespaced_name();
            let token_after = attempt.next();
            match (maybe_name, token_after) {
                // Non-empty attribute
                (Ok(ref name), Some(TokenTree::Punct(ref punct))) if punct.as_char() == '=' => {
                    self.commit(attempt);
                    let value;
                    {
                        // Parse a value under an attribute context
                        let in_attr = mem::replace(&mut self.in_attr, true);
                        value = self.markup()?;
                        self.in_attr = in_attr;
                    }
                    attrs.push(ast::Attribute {
                        name: name.clone(),
                        attr_type: ast::AttrType::Normal { value },
                    });
                },
                // Empty attribute
                (Ok(ref name), Some(TokenTree::Punct(ref punct))) if punct.as_char() == '?' => {
                    self.commit(attempt);
                    let toggler = self.attr_toggler();
                    attrs.push(ast::Attribute {
                        name: name.clone(),
                        attr_type: ast::AttrType::Empty { toggler },
                    });
                },
                // Class shorthand
                (Err(_), Some(TokenTree::Punct(ref punct))) if punct.as_char() == '.' => {
                    self.commit(attempt);
                    let name = self.name()?;
                    if let Some(toggler) = self.attr_toggler() {
                        classes_toggled.push((name, toggler));
                    } else {
                        classes_static.push(name);
                    }
                },
                // ID shorthand
                (Err(_), Some(TokenTree::Punct(ref punct))) if punct.as_char() == '#' => {
                    self.commit(attempt);
                    ids.push(self.name()?);
                },
                // If it's not a valid attribute, backtrack and bail out
                _ => break,
            }
        }
        Ok(ast::Attrs { classes_static, classes_toggled, ids, attrs })
    }

    /// Parses the `[cond]` syntax after an empty attribute or class shorthand.
    fn attr_toggler(&mut self) -> Option<ast::Toggler> {
        match self.peek() {
            Some(TokenTree::Group(ref group)) if group.delimiter() == Delimiter::Bracket => {
                self.advance();
                Some(ast::Toggler {
                    cond: group.stream(),
                    cond_span: group.span(),
                })
            },
            _ => None,
        }
    }

    /// Parses an identifier, without dealing with namespaces.
    fn name(&mut self) -> ParseResult<TokenStream> {
        let mut result = Vec::new();
        if let Some(token @ TokenTree::Ident(_)) = self.peek() {
            self.advance();
            result.push(token);
        } else {
            return self.error("expected identifier");
        }
        let mut expect_ident = false;
        loop {
            expect_ident = match self.peek() {
                Some(TokenTree::Punct(ref punct)) if punct.as_char() == '-' => {
                    self.advance();
                    result.push(TokenTree::Punct(punct.clone()));
                    true
                },
                Some(TokenTree::Ident(ref ident)) if expect_ident => {
                    self.advance();
                    result.push(TokenTree::Ident(ident.clone()));
                    false
                },
                _ => break,
            };
        }
        Ok(result.into_iter().collect())
    }

    /// Parses a HTML element or attribute name, along with a namespace
    /// if necessary.
    fn namespaced_name(&mut self) -> ParseResult<TokenStream> {
        let mut result = vec![self.name()?];
        if let Some(TokenTree::Punct(ref punct)) = self.peek() {
            if punct.as_char() == ':' {
                self.advance();
                result.push(TokenStream::from(TokenTree::Punct(punct.clone())));
                result.push(self.name()?);
            }
        }
        Ok(result.into_iter().collect())
    }

    /// Parses the given token stream as a Maud expression.
    fn block(&mut self, body: TokenStream, span: Span) -> ParseResult<ast::Block> {
        let markups = self.with_input(body).markups()?;
        Ok(ast::Block { markups, span })
    }
}
