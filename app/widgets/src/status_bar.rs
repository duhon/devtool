//! The status line of a run or a module: light, text, elapsed time, Stop, and Debug below.

#[cfg(target_arch = "wasm32")]
pub use view::{CSS, ConsoleStatus, ConsoleStatusProps};

#[cfg(target_arch = "wasm32")]
mod view {
    use crate::indicator::Indicator;
    use crate::tree::tree;
    use serde_json::Value;
    use wasm_bindgen::{JsCast, closure::Closure};
    use yew::prelude::*;

    pub const CSS: &str = include_str!("status_bar.css");

    #[derive(Properties, PartialEq)]
    pub struct ConsoleStatusProps {
        pub text: String,
        pub debug: Value,
        pub running: bool,
        pub started: Option<f64>,
        #[prop_or_default]
        pub onstop: Option<Callback<()>>,
        #[prop_or("console-stop".into())]
        pub stop_testid: String,
        #[prop_or("console-main-status".into())]
        pub testid: String,
        #[prop_or_default]
        pub waiting: bool,
        #[prop_or_default]
        pub state: Option<Indicator>,
    }

    #[function_component(ConsoleStatus)]
    pub fn console_status(props: &ConsoleStatusProps) -> Html {
        let expanded = use_state_eq(|| false);
        let clock = use_state_eq(js_sys::Date::now);
        {
            let clock = clock.clone();
            use_effect_with((props.running, props.started), move |(running, started)| {
                let timer = if *running && started.is_some() {
                    clock.set(js_sys::Date::now());
                    let callback =
                        Closure::<dyn FnMut()>::new(move || clock.set(js_sys::Date::now()));
                    match crate::dom::window()
                        .set_interval_with_callback_and_timeout_and_arguments_0(
                            callback.as_ref().unchecked_ref(),
                            100,
                        ) {
                        Ok(handle) => Some((handle, callback)),
                        Err(error) => {
                            web_sys::console::error_1(&error);
                            None
                        }
                    }
                } else {
                    None
                };
                move || {
                    if let Some((handle, _callback)) = timer {
                        crate::dom::window().clear_interval_with_handle(handle);
                    }
                }
            });
        }
        let toggle = {
            let expanded = expanded.clone();
            Callback::from(move |_| expanded.set(!*expanded))
        };
        let state = if props.waiting {
            Indicator::Warning
        } else {
            props
                .state
                .unwrap_or_else(|| Indicator::status(&props.text, props.running))
        };
        let outcome = match state {
            Indicator::Ready => "is-ready",
            Indicator::Running => "is-running",
            Indicator::Success => "is-ok",
            Indicator::Warning => "is-warning",
            Indicator::Error => "is-error",
        };
        html! {<>
            <div data-testid={props.testid.clone()} class={classes!("status-bar",outcome)} role="status" title="Click for debug info" onclick={toggle}>
                if props.running {
                    <span class="status-bar-spinner" aria-hidden="true"/>
                    if let Some(started)=props.started {
                        <span class="status-bar-elapsed" aria-hidden="true">{format!("{:.0} ms",(*clock-started).max(0.0))}</span>
                    }
                }
                <span class="status-bar-text">
                    {if props.text.is_empty(){"Ready"}else{&props.text}}
                </span>
                <button type="button" class="status-bar-stop" data-testid={props.stop_testid.clone()}
                    title="Stop this operation" aria-label="Stop this operation"
                    disabled={!props.running || props.onstop.is_none()}
                    onclick={{let onstop=props.onstop.clone();Callback::from(move |event:MouseEvent|{
                        event.stop_propagation();
                        if let Some(onstop)=&onstop {onstop.emit(());}
                    })}}>{"S"}</button>
            </div>
            <div data-testid={format!("{}-debug",props.testid)} class="tab-debug-body status-bar-debug" hidden={!*expanded}>{tree(&props.debug,0)}</div>
        </>}
    }
}
