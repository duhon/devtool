use super::{
    error,
    transport::{Reply, Transport},
    views::OperationStatus,
    window,
};
use serde_json::json;
use services_contracts::agent::{Catalog, FUSION_MODEL_ID, FUSION_MODEL_NAME, Model, Options};
use std::rc::Rc;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

const STORAGE_KEY: &str = "servicesConsole.agentOptions";

pub fn read_options() -> Result<Options, String> {
    let storage = window()
        .local_storage()
        .map_err(error)?
        .ok_or("Local storage is unavailable")?;
    storage
        .get_item(STORAGE_KEY)
        .map_err(error)?
        .map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|error| format!("Invalid Agent model settings: {error}"))
        })
        .transpose()
        .map(|options| options.unwrap_or_else(Options::for_new_chat))
}

#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    pub onstatus: Callback<OperationStatus>,
}

impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.transport, &other.transport) && self.onstatus == other.onstatus
    }
}

pub enum Msg {
    Load,
    Loaded(Reply),
    Query(String),
    Open,
    Close,
    Move(bool),
    Choose(String),
    Reasoning(String),
}

pub struct AgentSettings {
    catalog: Option<Catalog>,
    options: Options,
    query: String,
    filtering: bool,
    open: bool,
    selected: usize,
    loading: bool,
    started: f64,
    input: NodeRef,
}

fn category(model: &Model) -> &str {
    match model.category.as_deref().unwrap_or("") {
        "lightweight" => "Lightweight",
        "versatile" => "Versatile",
        "powerful" => "Powerful",
        "automatic" => "Automatic",
        other => other,
    }
}

impl AgentSettings {
    fn current(&self) -> Option<&Model> {
        let catalog = self.catalog.as_ref()?;
        let id = self
            .options
            .model
            .as_ref()
            .or(catalog.current.model.as_ref())?;
        catalog.models.iter().find(|model| &model.id == id)
    }

    fn label(&self) -> String {
        self.current()
            .map(|model| model.name.clone())
            .or_else(|| {
                self.options.model.as_ref().map(|id| {
                    if id == FUSION_MODEL_ID {
                        FUSION_MODEL_NAME.into()
                    } else {
                        id.clone()
                    }
                })
            })
            .or_else(|| {
                self.catalog
                    .as_ref()
                    .and_then(|catalog| catalog.current.model.clone())
            })
            .unwrap_or_else(|| FUSION_MODEL_NAME.into())
    }

    fn matches(&self) -> Vec<&Model> {
        let query = if self.filtering {
            self.query.to_lowercase()
        } else {
            String::new()
        };
        self.catalog
            .iter()
            .flat_map(|catalog| &catalog.models)
            .filter(|model| {
                model.enabled
                    && (model.name.to_lowercase().contains(&query)
                        || model.id.to_lowercase().contains(&query)
                        || category(model).to_lowercase().contains(&query))
            })
            .collect()
    }

    fn report(&self, ctx: &Context<Self>, operation: &str, result: Result<(), String>) {
        let reply = match result {
            Ok(()) => Reply::Result(json!({"options":self.options})),
            Err(error) => Reply::Error(json!({"error":error})),
        };
        ctx.props().onstatus.emit(OperationStatus::event(
            operation,
            self.started,
            Some(&reply),
        ));
    }

    fn save(&mut self, ctx: &Context<Self>, options: Options) {
        self.started = js_sys::Date::now();
        let result = (|| {
            let catalog = self.catalog.as_ref().ok_or("Load Copilot models first")?;
            options.validate(&catalog.models)?;
            let storage = window()
                .local_storage()
                .map_err(error)?
                .ok_or("Local storage is unavailable")?;
            storage
                .set_item(
                    STORAGE_KEY,
                    &serde_json::to_string(&options).map_err(|error| error.to_string())?,
                )
                .map_err(error)?;
            self.options = options;
            Ok(())
        })();
        self.query = self.label();
        self.open = false;
        self.report(ctx, "New Agent chat settings", result);
    }
}

impl Component for AgentSettings {
    type Message = Msg;
    type Properties = Props;

    fn create(ctx: &Context<Self>) -> Self {
        let options = read_options();
        if let Err(error) = &options {
            ctx.props().onstatus.emit(OperationStatus::event(
                "Agent settings",
                js_sys::Date::now(),
                Some(&Reply::Error(json!({"error":error}))),
            ));
        }
        ctx.link().send_message(Msg::Load);
        let mut settings = Self {
            catalog: None,
            options: options.unwrap_or_default(),
            query: String::new(),
            filtering: false,
            open: false,
            selected: 0,
            loading: false,
            started: 0.0,
            input: NodeRef::default(),
        };
        settings.query = settings.label();
        settings
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Load => {
                if self.loading {
                    return false;
                }
                self.loading = true;
                self.open = false;
                self.started = js_sys::Date::now();
                ctx.props().onstatus.emit(OperationStatus::event(
                    "Load Copilot models",
                    self.started,
                    None,
                ));
                ctx.props().transport.request(
                    json!({"service":"agent_models"}),
                    false,
                    ctx.link().callback(Msg::Loaded),
                );
            }
            Msg::Loaded(reply) => {
                self.loading = false;
                let result = match reply {
                    Reply::Result(value) => serde_json::from_value::<Catalog>(value)
                        .map_err(|error| format!("Invalid Copilot model catalog: {error}")),
                    Reply::Error(value) => Err(value["error"]
                        .as_str()
                        .unwrap_or("Could not load Copilot models")
                        .to_owned()),
                    _ => Err("Unexpected Copilot model catalog reply".into()),
                };
                let result = result.and_then(|catalog| {
                    if !catalog.models.iter().any(|model| model.enabled) {
                        return Err("No Copilot chat models are available for this account".into());
                    }
                    self.catalog = Some(catalog);
                    self.query = self.label();
                    let options = read_options()?;
                    options.validate(&self.catalog.as_ref().expect("loaded catalog").models)?;
                    self.options = options;
                    self.query = self.label();
                    Ok(())
                });
                self.report(ctx, "Load Copilot models", result);
            }
            Msg::Query(query) => {
                self.query = query;
                self.filtering = true;
                self.open = true;
                self.selected = 0;
            }
            Msg::Open => {
                self.open = true;
                self.filtering = false;
                self.selected = 0;
                if let Some(input) = self.input.cast::<HtmlInputElement>() {
                    input.select();
                }
            }
            Msg::Close => {
                self.open = false;
                self.query = self.label();
            }
            Msg::Move(down) => {
                let count = self.matches().len();
                self.open = true;
                if count > 0 {
                    self.selected = if down {
                        (self.selected + 1) % count
                    } else {
                        (self.selected + count - 1) % count
                    };
                }
            }
            Msg::Choose(id) => {
                self.save(
                    ctx,
                    Options {
                        model: Some(id),
                        reasoning_effort: None,
                    },
                );
            }
            Msg::Reasoning(effort) => {
                let model = self.options.model.clone().or_else(|| {
                    self.catalog
                        .as_ref()
                        .and_then(|catalog| catalog.current.model.clone())
                });
                self.save(
                    ctx,
                    Options {
                        model,
                        reasoning_effort: (!effort.is_empty()).then_some(effort),
                    },
                );
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let matches = self.matches();
        let open = self.open && !self.loading && self.catalog.is_some();
        let selected = matches.get(self.selected).map(|model| model.id.clone());
        let current = self.current();
        let efforts = current
            .map(|model| model.reasoning_efforts.as_slice())
            .unwrap_or_default();
        let default_effort = self
            .catalog
            .as_ref()
            .filter(|_| self.options.model.is_none())
            .and_then(|catalog| catalog.current.reasoning_effort.as_deref())
            .or_else(|| current.and_then(|model| model.default_reasoning_effort.as_deref()));
        let active = open
            .then(|| selected.clone())
            .flatten()
            .map(|id| format!("agent-model-{id}"));
        html! {
            <div class="agent-settings" data-testid="agent-settings">
                <strong>{"New Agent chats"}</strong>
                <label for="agent-model-input">{"Copilot model"}</label>
                <div class="agent-model-picker">
                    <div class="agent-model-current">
                        <input id="agent-model-input" data-testid="agent-model-input" ref={self.input.clone()}
                            role="combobox" autocomplete="off" aria-autocomplete="list"
                            aria-expanded={open.to_string()} aria-controls="agent-model-options" aria-activedescendant={active}
                            value={self.query.clone()} disabled={self.loading || self.catalog.is_none()}
                            onfocus={ctx.link().callback(|_|Msg::Open)} onblur={ctx.link().callback(|_|Msg::Close)}
                            oninput={ctx.link().callback(|event:InputEvent|Msg::Query(event.target_unchecked_into::<HtmlInputElement>().value()))}
                            onkeydown={ctx.link().batch_callback(move |event:KeyboardEvent| {
                                if event.is_composing() {return None;}
                                let message = match event.key().as_str() {
                                    "ArrowDown" => Some(Msg::Move(true)),
                                    "ArrowUp" => Some(Msg::Move(false)),
                                    "Escape" if open => Some(Msg::Close),
                                    "Enter" if open => selected.clone().map(Msg::Choose),
                                    _ => None,
                                };
                                if message.is_some() {
                                    event.prevent_default();
                                    event.stop_propagation();
                                }
                                if event.repeat() && event.key()=="Enter" {None} else {message}
                            })}/>
                        if let Some(model)=current {
                            <span class="agent-model-category" data-testid="agent-model-category">{category(model)}</span>
                        }
                    </div>
                    if open {
                        <div id="agent-model-options" role="listbox" aria-label="Copilot models" class="agent-model-options" data-testid="agent-model-options">
                            {for matches.iter().enumerate().map(|(index,model)| {
                                let id=model.id.clone();
                                html! {
                                    <button type="button" class={classes!("tab-button","agent-model-option",(index==self.selected).then_some("active"))}
                                        role="option" aria-selected={(index==self.selected).to_string()} id={format!("agent-model-{}",model.id)}
                                        data-model-id={model.id.clone()} title={model.id.clone()}
                                        onmousedown={Callback::from(|event:MouseEvent|event.prevent_default())}
                                        onclick={ctx.link().callback(move |_|Msg::Choose(id.clone()))}>
                                        <span>{&model.name}</span><span class="agent-model-category">{category(model)}</span>
                                    </button>
                                }
                            })}
                            if matches.is_empty() {<span class="agent-model-empty">{"No matching models"}</span>}
                        </div>
                    }
                </div>
                <label for="agent-reasoning">{"Reasoning"}</label>
                <select id="agent-reasoning" data-testid="agent-reasoning"
                    disabled={self.loading || efforts.is_empty()}
                    value={self.options.reasoning_effort.clone().unwrap_or_default()}
                    onchange={ctx.link().callback(|event:Event|Msg::Reasoning(event.target_unchecked_into::<HtmlSelectElement>().value()))}>
                    <option value="">{if efforts.is_empty() {"Not supported".to_owned()} else {default_effort.map(|effort|format!("Default ({effort})")).unwrap_or_else(||"Default".into())}}</option>
                    {for efforts.iter().map(|effort|html!{<option value={effort.clone()}>{effort}</option>})}
                </select>
            </div>
        }
    }
}
