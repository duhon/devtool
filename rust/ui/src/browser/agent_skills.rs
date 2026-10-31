//! Settings: the skills the Agent can use besides its own, from the installed modules and
//! the applied sources. Sources and modules are managed in their own settings.

use super::{
    transport::{Reply, Transport},
    views::OperationStatus,
};
use serde_json::{Value, json};
use std::rc::Rc;
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
}

pub struct AgentSkills {
    skills: Vec<Value>,
    problems: Vec<String>,
}

/// Where a skill comes from: `Module rest` or `Source Team`.
fn origin(skill: &Value) -> String {
    let origin = &skill["origin"];
    match origin["kind"].as_str() {
        Some("module") => format!("Module {}", origin["module"].as_str().unwrap_or_default()),
        Some("source") => format!("Source {}", origin["source"].as_str().unwrap_or_default()),
        _ => String::new(),
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
            skills: Vec::new(),
            problems: Vec::new(),
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        let Msg::Loaded(reply) = msg;
        match &reply {
            Reply::Result(value) => {
                self.skills = value["skills"].as_array().cloned().unwrap_or_default();
                self.problems = value["problems"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|problem| problem.as_str().map(str::to_owned))
                    .collect();
            }
            _ => ctx.props().onstatus.emit(OperationStatus::event(
                "Load Agent skills",
                js_sys::Date::now(),
                Some(&reply),
            )),
        }
        true
    }

    fn view(&self, _ctx: &Context<Self>) -> Html {
        html! {
            <div class="agent-settings agent-skills" data-testid="agent-skills">
                <strong>{"Agent skills"}</strong>
                <span class="agent-skills-hint">{"Skills of the installed modules and the applied sources. The Agent can use them from its next reply."}</span>
                if self.skills.is_empty() {
                    <span class="agent-skills-hint" data-testid="agent-skills-empty">{"No skills yet."}</span>
                } else {
                    <ul class="agent-skills-list" data-testid="agent-skills-list">
                        {for self.skills.iter().map(|skill| html! {
                            <li title={skill["folder"].as_str().unwrap_or_default().to_owned()} data-testid="agent-skill">
                                <span class="agent-skills-name">{skill["name"].as_str().unwrap_or_default()}</span>
                                <span class="settings-origin" data-testid="agent-skill-origin">{origin(skill)}</span>
                                <span class="agent-skills-description">{skill["description"].as_str().unwrap_or_default()}</span>
                            </li>
                        })}
                    </ul>
                }
                if !self.problems.is_empty() {
                    <ul class="agent-skills-problems" data-testid="agent-skills-problems">
                        {for self.problems.iter().map(|problem| html!{<li>{problem}</li>})}
                    </ul>
                }
            </div>
        }
    }
}
