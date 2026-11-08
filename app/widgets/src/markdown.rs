use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use yew::{prelude::*, virtual_dom::VTag};

fn append(stack: &mut [VTag], roots: &mut Vec<Html>, node: Html) {
    if let Some(parent) = stack.last_mut() {
        parent.add_child(node);
    } else {
        roots.push(node);
    }
}

fn close(stack: &mut Vec<VTag>, roots: &mut Vec<Html>) {
    let tag = stack.pop().expect("balanced Markdown parser events");
    append(stack, roots, tag.into());
}

pub fn render(text: &str) -> Html {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut stack: Vec<VTag> = Vec::new();
    let mut roots = Vec::new();
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => {
                let element = match tag {
                    Tag::Paragraph => VTag::new("p"),
                    Tag::Heading { level, .. } => VTag::new(match level {
                        HeadingLevel::H1 => "h1",
                        HeadingLevel::H2 => "h2",
                        HeadingLevel::H3 => "h3",
                        HeadingLevel::H4 => "h4",
                        HeadingLevel::H5 => "h5",
                        HeadingLevel::H6 => "h6",
                    }),
                    Tag::BlockQuote(_) => VTag::new("blockquote"),
                    Tag::CodeBlock(kind) => {
                        stack.push(VTag::new("pre"));
                        let mut code = VTag::new("code");
                        if let CodeBlockKind::Fenced(info) = kind
                            && let Some(language) = info.split_whitespace().next()
                        {
                            code.add_attribute("class", format!("language-{language}"));
                        }
                        code
                    }
                    Tag::List(start) => {
                        let mut list = VTag::new(if start.is_some() { "ol" } else { "ul" });
                        if let Some(start) = start {
                            list.add_attribute("start", start.to_string());
                        }
                        list
                    }
                    Tag::Item => VTag::new("li"),
                    Tag::Emphasis => VTag::new("em"),
                    Tag::Strong => VTag::new("strong"),
                    Tag::Strikethrough => VTag::new("del"),
                    Tag::Link {
                        dest_url, title, ..
                    } => {
                        let mut link = VTag::new("span");
                        if let Ok(url) = url::Url::parse(&dest_url)
                            && matches!(url.scheme(), "http" | "https" | "mailto")
                        {
                            link = VTag::new("a");
                            link.add_attribute("href", url.to_string());
                            link.add_attribute("target", "_blank");
                            link.add_attribute("rel", "noopener noreferrer");
                        }
                        if !title.is_empty() {
                            link.add_attribute("title", title.into_string());
                        }
                        link
                    }
                    Tag::Image { .. } => {
                        let mut image = VTag::new("span");
                        image.add_attribute("class", "agent-markdown-image");
                        image.add_attribute("title", "External image not loaded");
                        image
                    }
                    Tag::Table(_) => VTag::new("table"),
                    Tag::TableHead => {
                        stack.push(VTag::new("thead"));
                        VTag::new("tr")
                    }
                    Tag::TableRow => VTag::new("tr"),
                    Tag::TableCell => VTag::new(if stack.iter().any(|tag| tag.tag() == "thead") {
                        "th"
                    } else {
                        "td"
                    }),
                    _ => VTag::new("div"),
                };
                stack.push(element);
            }
            Event::End(tag) => {
                close(&mut stack, &mut roots);
                if matches!(tag, TagEnd::CodeBlock | TagEnd::TableHead) {
                    close(&mut stack, &mut roots);
                }
            }
            Event::Text(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text)
            | Event::FootnoteReference(text) => {
                append(&mut stack, &mut roots, Html::from(text.into_string()));
            }
            Event::Code(text) => {
                append(&mut stack, &mut roots, html! {<code>{text.as_ref()}</code>});
            }
            Event::SoftBreak => append(&mut stack, &mut roots, Html::from("\n")),
            Event::HardBreak => append(&mut stack, &mut roots, html! {<br/>}),
            Event::Rule => append(&mut stack, &mut roots, html! {<hr/>}),
            Event::TaskListMarker(checked) => append(
                &mut stack,
                &mut roots,
                html! {<input type="checkbox" disabled={true} {checked}/>},
            ),
        }
    }
    html! {<>{for roots}</>}
}
