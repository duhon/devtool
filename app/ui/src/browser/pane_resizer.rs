use super::error;
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{Element, HtmlElement, KeyboardEvent, PointerEvent, ResizeObserver};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub panel: AttrValue,
    pub minimum: u32,
    pub width: Option<u32>,
    pub onwidth: Callback<u32>,
    pub onerror: Callback<String>,
}

pub enum Msg {
    Bounds,
    Start(PointerEvent),
    Move(PointerEvent),
    End(PointerEvent),
    Cancel,
    Key(KeyboardEvent),
}

struct Drag {
    id: i32,
    x: i32,
    width: u32,
    previous: Option<u32>,
}

pub struct PaneResizer {
    handle: NodeRef,
    width: Option<u32>,
    maximum: u32,
    drag: Option<Drag>,
    observer: Option<ResizeObserver>,
    callback: Option<Closure<dyn FnMut()>>,
}

impl PaneResizer {
    fn workspace(&self) -> Result<HtmlElement, String> {
        self.handle
            .cast::<Element>()
            .ok_or("Missing panel resize handle".to_owned())?
            .closest(".console-workspace")
            .map_err(error)?
            .ok_or("Missing Console workspace".to_owned())?
            .dyn_into::<HtmlElement>()
            .map_err(|_| "Console workspace is not an HTML element".to_owned())
    }

    fn bounds(&mut self, ctx: &Context<Self>) -> Result<(), String> {
        let workspace = self.workspace()?;
        let width = |selector| -> Result<i32, String> {
            Ok(workspace
                .query_selector(selector)
                .map_err(error)?
                .ok_or_else(|| format!("Missing Console layout element: {selector}"))?
                .get_bounding_client_rect()
                .width()
                .ceil() as i32)
        };
        self.maximum = (workspace.client_width()
            - width(".console-check-menu")?
            - width(".console-toolbar")?
            - 160)
            .clamp(ctx.props().minimum as i32, 16_384) as u32;
        self.apply(ctx)
    }

    fn apply(&self, ctx: &Context<Self>) -> Result<(), String> {
        let style = self.workspace()?.style();
        style
            .set_property("--console-pane-min", &format!("{}px", ctx.props().minimum))
            .map_err(error)?;
        let width = self
            .width
            .map(|width| format!("{}px", width.clamp(ctx.props().minimum, self.maximum)))
            .unwrap_or_else(|| "1fr".into());
        style
            .set_property("--console-pane-width", &width)
            .map_err(error)
    }

    fn current_width(&self) -> Result<u32, String> {
        Ok(self
            .handle
            .cast::<Element>()
            .and_then(|handle| handle.parent_element())
            .ok_or("Missing right panel".to_owned())?
            .get_bounding_client_rect()
            .width()
            .round() as u32)
    }

    fn observe(&mut self, ctx: &Context<Self>) -> Result<(), String> {
        let workspace = self.workspace()?;
        self.bounds(ctx)?;
        let targets = [".console-check-menu", ".console-toolbar"]
            .into_iter()
            .map(|selector| {
                workspace
                    .query_selector(selector)
                    .map_err(error)?
                    .ok_or_else(|| format!("Missing Console layout element: {selector}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let link = ctx.link().clone();
        let callback = Closure::<dyn FnMut()>::new(move || link.send_message(Msg::Bounds));
        let observer = ResizeObserver::new(callback.as_ref().unchecked_ref()).map_err(error)?;
        for target in &targets {
            observer.observe(target);
        }
        observer.observe(&workspace);
        self.observer = Some(observer);
        self.callback = Some(callback);
        Ok(())
    }
}

impl Component for PaneResizer {
    type Message = Msg;
    type Properties = Props;

    fn create(ctx: &Context<Self>) -> Self {
        Self {
            handle: NodeRef::default(),
            width: ctx.props().width,
            maximum: 16_384,
            drag: None,
            observer: None,
            callback: None,
        }
    }

    fn rendered(&mut self, ctx: &Context<Self>, first: bool) {
        if first && let Err(error) = self.observe(ctx) {
            ctx.props().onerror.emit(error);
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        if ctx.props().width != old.width {
            self.width = ctx.props().width;
            self.drag = None;
            if let Err(error) = self.bounds(ctx) {
                ctx.props().onerror.emit(error);
            }
        }
        true
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        let result = (|| -> Result<bool, String> {
            match msg {
                Msg::Bounds => self.bounds(ctx)?,
                Msg::Start(event) => {
                    if event.button() != 0 {
                        return Ok(false);
                    }
                    event.prevent_default();
                    self.bounds(ctx)?;
                    self.handle
                        .cast::<Element>()
                        .ok_or("Missing panel resize handle".to_owned())?
                        .set_pointer_capture(event.pointer_id())
                        .map_err(error)?;
                    self.drag = Some(Drag {
                        id: event.pointer_id(),
                        x: event.client_x(),
                        width: self.current_width()?,
                        previous: self.width,
                    });
                }
                Msg::Move(event) => {
                    let Some(drag) = &self.drag else {
                        return Ok(false);
                    };
                    if event.pointer_id() != drag.id {
                        return Ok(false);
                    }
                    self.width = Some(
                        (i64::from(drag.width) + i64::from(drag.x) - i64::from(event.client_x()))
                            .clamp(i64::from(ctx.props().minimum), i64::from(self.maximum))
                            as u32,
                    );
                    self.apply(ctx)?;
                }
                Msg::End(event) => {
                    if self
                        .drag
                        .as_ref()
                        .is_none_or(|drag| drag.id != event.pointer_id())
                    {
                        return Ok(false);
                    }
                    self.drag = None;
                    if let Some(width) = self.width {
                        ctx.props().onwidth.emit(width);
                    }
                }
                Msg::Cancel => {
                    let Some(drag) = self.drag.take() else {
                        return Ok(false);
                    };
                    self.width = drag.previous;
                    self.apply(ctx)?;
                }
                Msg::Key(event) => {
                    self.bounds(ctx)?;
                    let width = self.current_width()?;
                    let next = match event.key().as_str() {
                        "ArrowLeft" => width.saturating_add(8).min(self.maximum),
                        "ArrowRight" => width.saturating_sub(8).max(ctx.props().minimum),
                        "Home" => ctx.props().minimum,
                        "End" => self.maximum,
                        _ => return Ok(false),
                    };
                    event.prevent_default();
                    self.width = Some(next);
                    self.apply(ctx)?;
                    ctx.props().onwidth.emit(next);
                }
            }
            Ok(true)
        })();
        match result {
            Ok(changed) => changed,
            Err(error) => {
                ctx.props().onerror.emit(error);
                false
            }
        }
    }

    fn destroy(&mut self, _: &Context<Self>) {
        if let Some(observer) = &self.observer {
            observer.disconnect();
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {
            <div ref={self.handle.clone()} class="console-pane-resizer" data-testid="console-pane-resizer"
                data-panel={ctx.props().panel.clone()} role="separator" tabindex="0"
                aria-label="Resize right panel" aria-orientation="vertical"
                aria-valuemin={ctx.props().minimum.to_string()} aria-valuemax={self.maximum.to_string()}
                aria-valuenow={self.width.map(|width|width.clamp(ctx.props().minimum,self.maximum).to_string())}
                onpointerdown={ctx.link().callback(Msg::Start)} onpointermove={ctx.link().callback(Msg::Move)}
                onpointerup={ctx.link().callback(Msg::End)} onpointercancel={ctx.link().callback(|_|Msg::Cancel)}
                onkeydown={ctx.link().callback(Msg::Key)}/>
        }
    }
}
