use super::{error, views::indicator, window};
use crate::activity::Indicator;
use web_sys::{Element, KeyboardEvent, PointerEvent};
use yew::prelude::*;

const MIN_WIDTH: u32 = 28;
const MAX_WIDTH: u32 = 320;

#[derive(Clone, PartialEq)]
pub struct Item {
    pub line: usize,
    pub title: String,
    pub state: Indicator,
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub width: u32,
    pub onwidth: Callback<u32>,
    pub items: Vec<Item>,
    pub active: Option<usize>,
    pub onselect: Callback<usize>,
    pub onerror: Callback<String>,
}

pub enum Msg {
    Start(PointerEvent),
    Move(PointerEvent),
    End(PointerEvent),
    Cancel,
    Key(KeyboardEvent),
}

pub struct CheckRail {
    width: u32,
    drag: Option<(i32, i32, u32)>,
    handle: NodeRef,
}

fn maximum() -> u32 {
    window()
        .inner_width()
        .ok()
        .and_then(|width| width.as_f64())
        .map(|width| (width * 0.4) as u32)
        .unwrap_or(MAX_WIDTH)
        .clamp(MIN_WIDTH, MAX_WIDTH)
}

impl Component for CheckRail {
    type Message = Msg;
    type Properties = Props;

    fn create(ctx: &Context<Self>) -> Self {
        Self {
            width: ctx.props().width.clamp(MIN_WIDTH, MAX_WIDTH),
            drag: None,
            handle: NodeRef::default(),
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        if ctx.props().width != old.width {
            self.width = ctx.props().width.clamp(MIN_WIDTH, MAX_WIDTH);
            self.drag = None;
        }
        true
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Start(event) => {
                if event.button() != 0 {
                    return false;
                }
                event.prevent_default();
                let result = self
                    .handle
                    .cast::<Element>()
                    .ok_or("Missing check-menu resize handle".to_owned())
                    .and_then(|target| {
                        target
                            .set_pointer_capture(event.pointer_id())
                            .map_err(error)
                    });
                if let Err(error) = result {
                    ctx.props().onerror.emit(error);
                    return false;
                }
                self.drag = Some((
                    event.pointer_id(),
                    event.client_x(),
                    self.width.min(maximum()),
                ));
            }
            Msg::Move(event) => {
                let Some((id, x, width)) = self.drag else {
                    return false;
                };
                if event.pointer_id() != id {
                    return false;
                }
                let width = (width as i64 + i64::from(event.client_x()) - i64::from(x))
                    .clamp(i64::from(MIN_WIDTH), i64::from(maximum()))
                    as u32;
                if self.width == width {
                    return false;
                }
                self.width = width;
            }
            Msg::End(event) => {
                if self.drag.is_none_or(|(id, _, _)| id != event.pointer_id()) {
                    return false;
                }
                self.drag = None;
                ctx.props().onwidth.emit(self.width);
            }
            Msg::Cancel => {
                if let Some((_, _, width)) = self.drag.take() {
                    self.width = width;
                } else {
                    return false;
                }
            }
            Msg::Key(event) => {
                let width = match event.key().as_str() {
                    "ArrowLeft" => self.width.saturating_sub(8).max(MIN_WIDTH),
                    "ArrowRight" => self.width.saturating_add(8).min(maximum()),
                    "Home" => MIN_WIDTH,
                    "End" => maximum(),
                    _ => return false,
                };
                event.prevent_default();
                self.width = width;
                ctx.props().onwidth.emit(self.width);
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let width = self.width.min(maximum());
        html! {
            <nav class={classes!("console-check-menu",(width>MIN_WIDTH).then_some("is-expanded"))}
                style={format!("width:{width}px")} aria-label="Checks" data-testid="console-check-menu">
                <div class="console-check-links">
                    {for ctx.props().items.iter().enumerate().map(|(index,item)| {
                        let line=item.line;
                        let title=if item.title.is_empty(){format!("Check {} · line {}",index+1,line+1)}
                            else{format!("{} · line {}",item.title,line+1)};
                        html!{<button type="button" class={classes!("console-check-link",(ctx.props().active==Some(line)).then_some("active"))}
                            aria-current={if ctx.props().active==Some(line){"location"}else{"false"}} {title}
                            data-line={(line+1).to_string()} onclick={ctx.props().onselect.reform(move |_|line)}>
                            <span class="console-check-number">{index+1}</span>
                            <span class="console-check-comment">{item.title.clone()}</span>
                            {indicator(item.state,format!("Check {} · {}",index+1,item.state.name()))}
                        </button>}
                    })}
                </div>
                <div ref={self.handle.clone()} class="console-check-resizer" data-testid="console-check-resizer" role="separator" tabindex="0"
                    aria-label="Resize check menu" aria-orientation="vertical" aria-valuemin={MIN_WIDTH.to_string()}
                    aria-valuemax={maximum().to_string()} aria-valuenow={width.to_string()}
                    onpointerdown={ctx.link().callback(Msg::Start)} onpointermove={ctx.link().callback(Msg::Move)}
                    onpointerup={ctx.link().callback(Msg::End)} onpointercancel={ctx.link().callback(|_|Msg::Cancel)}
                    onkeydown={ctx.link().callback(Msg::Key)}/>
            </nav>
        }
    }
}
