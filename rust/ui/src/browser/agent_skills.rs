use super::{
    transport::{Reply, Transport},
    views::OperationStatus,
};
use serde_json::{Value, json};
use std::rc::Rc;
use web_sys::HtmlInputElement;
use yew::prelude::*;

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
    Loaded(Reply),
    Input(String),
    Save(String),
    Saved(f64, Reply),
}

/// Settings for the team skills folder that every following Agent reply can use.
pub struct AgentSkills {
    input: String,
    saved: Option<String>,
    skills: Vec<Value>,
    problems: Vec<String>,
    busy: bool,
    loaded: bool,
    /// The last save was rejected; cleared once the folder is edited or saved.
    invalid: bool,
}

impl AgentSkills {
    fn apply(&mut self, value: &Value) {
        self.saved = value["teamFolder"].as_str().map(str::to_owned);
        self.input = self.saved.clone().unwrap_or_default();
        self.skills = value["skills"].as_array().cloned().unwrap_or_default();
        self.problems = value["problems"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|problem| problem.as_str().map(str::to_owned))
            .collect();
    }
}

impl Component for AgentSkills {
    type Message = Msg;
    type Properties = Props;

    fn create(ctx: &Context<Self>) -> Self {
        ctx.props().transport.request(
            json!({"service":"agent_skills","op":"get"}),
            false,
            ctx.link().callback(Msg::Loaded),
        );
        Self {
            input: String::new(),
            saved: None,
            skills: Vec::new(),
            problems: Vec::new(),
            busy: true,
            loaded: false,
            invalid: false,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Loaded(reply) => {
                self.busy = false;
                self.loaded = true;
                match &reply {
                    Reply::Result(value) => self.apply(value),
                    _ => ctx.props().onstatus.emit(OperationStatus::event(
                        "Load Agent skills",
                        js_sys::Date::now(),
                        Some(&reply),
                    )),
                }
            }
            Msg::Input(text) => {
                self.input = text;
                self.invalid = false;
            }
            Msg::Save(folder) => {
                if self.busy {
                    return false;
                }
                self.busy = true;
                let started = js_sys::Date::now();
                ctx.props().onstatus.emit(OperationStatus::event(
                    "Save team skills folder",
                    started,
                    None,
                ));
                ctx.props().transport.request(
                    json!({"service":"agent_skills","op":"save","teamFolder":folder}),
                    false,
                    ctx.link().callback(move |reply| Msg::Saved(started, reply)),
                );
            }
            Msg::Saved(started, reply) => {
                self.busy = false;
                let mut status =
                    OperationStatus::event("Save team skills folder", started, Some(&reply));
                match &reply {
                    Reply::Result(value) => {
                        self.apply(value);
                        self.invalid = false;
                        status.text = if self.saved.is_none() {
                            "Team skills folder cleared".into()
                        } else {
                            let count = self.skills.len();
                            let found = format!(
                                "Team skills folder saved · {count} skill{}",
                                if count == 1 { "" } else { "s" }
                            );
                            match value["warning"].as_str() {
                                Some(warning) => format!("{found} · WARNING · {warning}"),
                                None => found,
                            }
                        };
                    }
                    _ => self.invalid = true,
                }
                ctx.props().onstatus.emit(status);
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let input = self.input.clone();
        let changed = input.trim() != self.saved.as_deref().unwrap_or_default();
        html! {
            <div class="agent-settings agent-skills" data-testid="agent-skills">
                <strong>{"Agent skills"}</strong>
                <label for="agent-team-skills-input">{"Team skills folder"}</label>
                <div class="agent-skills-folder">
                    <input id="agent-team-skills-input" data-testid="agent-team-skills-input"
                        class={classes!(self.invalid.then_some("invalid"))} aria-invalid={self.invalid.to_string()}
                        placeholder="/path/to/team-skills" spellcheck="false" autocomplete="off"
                        value={self.input.clone()} disabled={!self.loaded}
                        oninput={ctx.link().callback(|event:InputEvent|Msg::Input(event.target_unchecked_into::<HtmlInputElement>().value()))}
                        onkeydown={ctx.link().batch_callback(move |event:KeyboardEvent| {
                            (event.key()=="Enter" && !event.is_composing() && !event.repeat()).then(|| {
                                event.prevent_default();
                                Msg::Save(event.target_unchecked_into::<HtmlInputElement>().value())
                            })
                        })}/>
                    <button type="button" class="tab-button" data-testid="agent-team-skills-save"
                        disabled={self.busy || !changed}
                        onclick={ctx.link().callback(move |_|Msg::Save(input.clone()))}>{"Save"}</button>
                    if self.saved.is_some() {
                        <button type="button" class="tab-button" data-testid="agent-team-skills-clear" disabled={self.busy}
                            onclick={ctx.link().callback(|_|Msg::Save(String::new()))}>{"Clear"}</button>
                    }
                </div>
                <span class="agent-skills-hint">{"A local folder, such as a team repository checkout, with one folder per skill containing SKILL.md. The Agent can use them from its next reply."}</span>
                if !self.skills.is_empty() {
                    <ul class="agent-skills-list" data-testid="agent-team-skills-list">
                        {for self.skills.iter().map(|skill| html! {
                            <li title={skill["folder"].as_str().unwrap_or_default().to_owned()}>
                                <span class="agent-skills-name">{skill["name"].as_str().unwrap_or_default()}</span>
                                <span class="agent-skills-description">{skill["description"].as_str().unwrap_or_default()}</span>
                            </li>
                        })}
                    </ul>
                }
                if !self.problems.is_empty() {
                    <ul class="agent-skills-problems" data-testid="agent-team-skills-problems">
                        {for self.problems.iter().map(|problem| html!{<li>{problem}</li>})}
                    </ul>
                }
            </div>
        }
    }
}
