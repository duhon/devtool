//! SDK for Services DevTools modules: wasm services and panels the Kernel loads.
//!
//! A module is a [`Module`] (its model) exported with [`export!`]. The Kernel sends it
//! events ([`Event`]) and gets back commands ([`Cmd`]) and, while it is shown, its UI tree
//! ([`ui::Node`]); editor, service and panel blocks are synchronous calls. The `kind` in the
//! manifest says which blocks the Kernel uses: a service answers `build` and `render`, a
//! panel may hook `before_request` and `after_response`; both may contribute to the editor.
//! Everything crosses the boundary as protobuf the SDK encodes.

pub mod cmd;
pub mod driver;
pub mod grpc;
pub mod http;
pub mod lang;
pub mod manifest;
pub mod mysql;
pub mod process;
pub mod refs;
pub mod run;
pub mod tcp;
pub mod time;
pub mod ui;
pub mod wire;

use prost::Message;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::cell::RefCell;
use wire::{answer, event, request};

pub use manifest::{Kind, Manifest};
pub use wire::{
    Check, Chunk, Cmd, ColumnType, Completions, DriverCall, Failure, Fold, Item, Line, LineColumns,
    Rendered, Section, Subscriptions, Token,
};

/// A text with its `{{…}}` variables resolved, and the values of the sensitive ones in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolution {
    pub text: String,
    pub secrets: Vec<String>,
}

/// The ABI of the exports [`export!`] generates; the Kernel loads only an exact match.
pub const ABI: u32 = 1;

/// An event from the Kernel with the module's own UI message decoded.
#[derive(Clone, Debug, PartialEq)]
pub enum Event<M> {
    Init {
        settings: Value,
    },
    /// An action in the module's UI; `value` is the field's text or choice.
    Ui {
        msg: M,
        value: Option<String>,
    },
    Reply {
        tag: String,
        result: Result<Value, Failure>,
    },
    End {
        tag: String,
    },
    /// The document of the service changed (debounced, the changed check only).
    Document(Check),
    /// The user asked to discover again for this check, cached or not.
    Discover(Check),
    /// The user asked for hints at `pos` (a UTF-8 byte offset in the check text): completion
    /// opened on demand, or a typed character that may commit an address. A module that
    /// completes from fetched data fetches it now.
    Hints {
        check: Check,
        pos: u32,
    },
    Request {
        id: String,
        service: String,
        finished: bool,
    },
    /// A session was restored.
    Workspace,
    /// A watched kv key changed.
    Storage {
        key: String,
    },
    Shown,
    Hidden,
    Modules,
    ToolCall {
        tag: String,
        name: String,
        args: Value,
    },
    /// Run `check` (for a module that subscribes `run`): call drivers, then finish with
    /// [`cmd::result`] under `tag`. Commands sent while handling this event or a reply to
    /// a call made for it belong to the run.
    Run {
        tag: String,
        check: Check,
    },
    /// The user stopped the run `tag`. Its locks are still held while this is handled; then
    /// the Kernel cancels the run's calls and releases them.
    Cancel {
        tag: String,
    },
}

thread_local! {
    static NOW: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// When the Kernel sent the event being handled, in milliseconds since 1970.
pub fn now() -> f64 {
    NOW.with(std::cell::Cell::get)
}

/// A module: its model, how it changes, and the blocks the Kernel calls synchronously.
/// Synchronous calls read the model and must not change it; they should stay within a few
/// milliseconds.
pub trait Module: Default + 'static {
    /// The module's own UI messages.
    type Msg: Serialize + DeserializeOwned;

    fn update(&mut self, event: Event<Self::Msg>) -> Vec<Cmd> {
        let _ = event;
        Vec::new()
    }
    /// The UI tree while the panel is shown; a service draws its Options form here.
    fn view(&self) -> Option<ui::Node> {
        None
    }
    fn subscriptions(&self) -> Subscriptions {
        Subscriptions::default()
    }

    /// Tokens of `lines` given the state before the first; each line ends with its state.
    /// `columns` are the response column types (`render` metadata) of response markers
    /// among the lines.
    fn highlight(&self, state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
        let _ = columns;
        lines
            .iter()
            .map(|_| Line {
                tokens: Vec::new(),
                state: state.to_vec(),
            })
            .collect()
    }
    fn complete(&self, check: &Check, pos: u32) -> Completions {
        let _ = check;
        Completions {
            from: pos,
            to: pos,
            items: Vec::new(),
        }
    }
    fn fold(&self, check: &Check) -> Vec<Fold> {
        let _ = check;
        Vec::new()
    }

    /// The driver call a check runs.
    fn build(&self, check: &Check) -> Result<DriverCall, String> {
        let _ = check;
        Err("This module runs no requests".into())
    }
    /// The response text of a driver answer or a piece of a stream.
    fn render(&self, chunk: &Chunk) -> Result<Rendered, String> {
        let _ = chunk;
        Err("This module renders no responses".into())
    }
    fn export(&self, check: &Check) -> Result<String, String> {
        let _ = check;
        Err("Export isn't supported".into())
    }
    /// A check's text from an exported command.
    fn import(&self, text: &str) -> Result<String, String> {
        let _ = text;
        Err("Import isn't supported".into())
    }

    /// Changes or rejects a call before any driver runs it; panels run in `order`.
    fn before_request(&self, call: DriverCall) -> Result<DriverCall, String> {
        Ok(call)
    }
    /// What a finished run hands panels: `call` is the driver call (empty for a check the
    /// module ran itself) and `response` the run record (see the SDK docs).
    fn after_response(&self, call: &DriverCall, response: &Value) -> Vec<Cmd> {
        let _ = (call, response);
        Vec::new()
    }
    /// `text` with the `{{…}}` variables this panel owns resolved (see
    /// [`wire::ResolveRequest`]; [`refs::replace`] helps): `None` when it keeps no variables.
    /// In `json` mode each value is escaped as the content of a JSON string.
    fn resolve(&self, text: &str, json: bool, last: bool) -> Option<Result<Resolution, String>> {
        let _ = (text, json, last);
        None
    }
}

fn decode_event<M: DeserializeOwned>(event: wire::Event) -> Result<Event<M>, String> {
    let kind = event.kind.ok_or("An empty event")?;
    Ok(match kind {
        event::Kind::Init(settings) => Event::Init {
            settings: wire::value(&settings),
        },
        event::Kind::Ui(ui) => Event::Ui {
            msg: serde_json::from_slice(&ui.msg)
                .map_err(|error| format!("Unknown UI message: {error}"))?,
            value: ui.value,
        },
        event::Kind::Reply(reply) => Event::Reply {
            tag: reply.tag,
            result: match (reply.ok, reply.error) {
                (_, Some(error)) => Err(error),
                (ok, None) => Ok(wire::value(&ok.unwrap_or_default())),
            },
        },
        event::Kind::End(tag) => Event::End { tag },
        event::Kind::Document(check) => Event::Document(check),
        event::Kind::Discover(check) => Event::Discover(check),
        event::Kind::Hints(hints) => Event::Hints {
            check: hints.check.unwrap_or_default(),
            pos: hints.pos,
        },
        event::Kind::Request(request) => Event::Request {
            id: request.id,
            service: request.service,
            finished: request.finished,
        },
        event::Kind::Workspace(_) => Event::Workspace,
        event::Kind::Storage(key) => Event::Storage { key },
        event::Kind::Shown(_) => Event::Shown,
        event::Kind::Hidden(_) => Event::Hidden,
        event::Kind::Modules(_) => Event::Modules,
        event::Kind::ToolCall(call) => Event::ToolCall {
            tag: call.tag,
            name: call.name,
            args: wire::value(&call.args),
        },
        event::Kind::Run(run) => Event::Run {
            tag: run.tag,
            check: run.check.unwrap_or_default(),
        },
        event::Kind::Cancel(tag) => Event::Cancel { tag },
    })
}

fn handle<T: Module>(state: &RefCell<Option<T>>, request: &[u8]) -> Result<answer::Kind, String> {
    let kind = wire::Request::decode(request)
        .map_err(|error| format!("Invalid request: {error}"))?
        .kind
        .ok_or("An empty request")?;
    let mut state = state.borrow_mut();
    let module = state.get_or_insert_with(T::default);
    Ok(match kind {
        request::Kind::Update(update) => {
            let event = update.event.unwrap_or_default();
            NOW.with(|now| now.set(event.now));
            let event = decode_event(event)?;
            let cmds = module.update(event);
            answer::Kind::Updated(wire::Updated {
                cmds,
                view: if update.view { module.view() } else { None },
                subscriptions: Some(module.subscriptions()),
            })
        }
        request::Kind::View(_) => answer::Kind::View(wire::OptionalNode {
            node: module.view(),
        }),
        request::Kind::Highlight(highlight) => answer::Kind::Highlighted(wire::Highlighted {
            lines: module.highlight(&highlight.state, &highlight.lines, &highlight.columns),
        }),
        request::Kind::Complete(complete) => answer::Kind::Completions(
            module.complete(&complete.check.unwrap_or_default(), complete.pos),
        ),
        request::Kind::Fold(check) => answer::Kind::Folds(wire::Folds {
            folds: module.fold(&check),
        }),
        request::Kind::Build(check) => answer::Kind::Call(module.build(&check)?),
        request::Kind::Render(chunk) => answer::Kind::Rendered(module.render(&chunk)?),
        request::Kind::Export(check) => answer::Kind::Text(module.export(&check)?),
        request::Kind::Import(text) => answer::Kind::Text(module.import(&text)?),
        request::Kind::BeforeRequest(call) => answer::Kind::Call(module.before_request(call)?),
        request::Kind::AfterResponse(after) => answer::Kind::Cmds(wire::Cmds {
            cmds: module.after_response(
                &after.call.unwrap_or_default(),
                &wire::value(&after.response),
            ),
        }),
        request::Kind::Resolve(resolve) => answer::Kind::Resolved(
            match module.resolve(&resolve.text, resolve.json, resolve.last) {
                None => wire::Resolved::default(),
                Some(Ok(resolution)) => wire::Resolved {
                    handled: true,
                    text: resolution.text,
                    error: None,
                    secrets: resolution.secrets,
                },
                Some(Err(error)) => wire::Resolved {
                    handled: true,
                    error: Some(error),
                    ..Default::default()
                },
            },
        ),
    })
}

/// Answers one encoded [`wire::Request`] with an encoded [`wire::Answer`]; `state` holds
/// the model, created on first use.
pub fn serve<T: Module>(state: &RefCell<Option<T>>, request: &[u8]) -> Vec<u8> {
    let kind = handle(state, request).unwrap_or_else(answer::Kind::Error);
    wire::Answer { kind: Some(kind) }.encode_to_vec()
}

/// A module linked into the same program: the Kernel's tests run modules without wasm.
pub struct Linked<T>(RefCell<Option<T>>);

impl<T: Module> Default for Linked<T> {
    fn default() -> Self {
        Self(RefCell::new(None))
    }
}

impl<T: Module> Linked<T> {
    pub fn call(&self, request: &[u8]) -> Vec<u8> {
        serve(&self.0, request)
    }
}

/// Exports a module from its wasm: `export!(MyModule)` where the type implements [`Module`].
/// The crate is a `cdylib` built for `wasm32-unknown-unknown`.
///
/// Exports: `sdk_abi() -> u32`; `sdk_alloc(len) -> ptr` for a request; `sdk_call(ptr, len)
/// -> answer` consumes the request and returns a little-endian `u32` length followed by the
/// answer bytes; `sdk_free(answer)`.
#[macro_export]
macro_rules! export {
    ($module:ty) => {
        #[cfg(target_arch = "wasm32")]
        ::std::thread_local! {
            static __SERVICES_SDK_STATE: ::std::cell::RefCell<::std::option::Option<$module>> =
                const { ::std::cell::RefCell::new(::std::option::Option::None) };
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn sdk_abi() -> u32 {
            $crate::ABI
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn sdk_alloc(len: usize) -> *mut u8 {
            ::std::boxed::Box::into_raw(::std::vec![0u8; len].into_boxed_slice()).cast()
        }

        /// # Safety
        /// `pointer` and `len` come from `sdk_alloc` with `len` bytes written.
        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn sdk_call(pointer: *mut u8, len: usize) -> *mut u8 {
            let request = unsafe {
                ::std::boxed::Box::from_raw(::std::ptr::slice_from_raw_parts_mut(pointer, len))
            };
            let answer = __SERVICES_SDK_STATE.with(|state| $crate::serve::<$module>(state, &request));
            let mut framed = ::std::vec::Vec::with_capacity(4 + answer.len());
            framed.extend_from_slice(&(answer.len() as u32).to_le_bytes());
            framed.extend_from_slice(&answer);
            ::std::boxed::Box::into_raw(framed.into_boxed_slice()).cast()
        }

        /// # Safety
        /// `answer` comes from `sdk_call` and is freed once.
        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn sdk_free(answer: *mut u8) {
            let len = u32::from_le_bytes(unsafe { *(answer as *const [u8; 4]) }) as usize;
            drop(unsafe {
                ::std::boxed::Box::from_raw(::std::ptr::slice_from_raw_parts_mut(answer, 4 + len))
            });
        }
    };
}
