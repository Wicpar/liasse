//! The wasm-bindgen boundary: a thin capability-free shell over the pure core.
//!
//! Everything here only marshals between `JsValue` and the [`crate::ClientReplica`]
//! / [`crate::request`] logic — the §12.2 apply and the wire schema live one layer
//! down, in `liasse-wire`, so this module reconstructs no semantics. It holds no
//! authority: it never mints or checks a token, and it never fetches. The TS shell
//! attaches the connection token and the §12.3 operation-id headers and performs the
//! transport; the wasm core parses already-authorized frames and serializes request
//! bodies.
//!
//! No-panic discipline holds at the boundary (AGENTS.md): every fallible marshal
//! returns `Result<_, JsValue>`, so a malformed value becomes a JS error, never an
//! abort. Values cross as JSON text through `JSON.parse`/`stringify`, which preserves
//! the arbitrary-precision numbers liasse-wire carries.

use js_sys::JSON;
use serde::Serialize;
use wasm_bindgen::prelude::*;

use liasse_wire::serde_json;
use liasse_wire::{OperationId, Sub, Value, WireWindow};

use crate::error::CoreError;
use crate::replica::ClientReplica;
use crate::request;

/// The client's per-connection §12.2 replica, exposed to JS. It folds decoded
/// downstream frames into a per-subscription replica and answers the shell's reads;
/// it carries no authority.
#[wasm_bindgen]
pub struct WireClient {
    replica: ClientReplica,
}

#[wasm_bindgen]
impl WireClient {
    /// A fresh client with no subscriptions.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self { replica: ClientReplica::new() }
    }

    /// Fold one downstream SSE frame into the replica. `data` is the frame JSON (the
    /// SSE `data:` payload); `frontier` is the SSE `id:` (empty when the frame carries
    /// none). Returns the effect as a JS object (`{ kind, sub?, frontier?, rows?,
    /// scalar?, close_reason?, reset_reason?, fault? }`).
    ///
    /// # Errors
    /// A JS error string if the frame is malformed, targets an unopened subscription,
    /// or does not fit the subscription's state — never a panic.
    #[wasm_bindgen(js_name = applyFrame)]
    pub fn apply_frame(&mut self, data: &str, frontier: &str) -> Result<JsValue, JsValue> {
        let applied = self.replica.apply(data, frontier).map_err(core_err)?;
        to_js(&applied)
    }

    /// The rows subscription `sub` currently holds, as an array of `{ id, value }`.
    ///
    /// # Errors
    /// A JS error only if the rows cannot be marshaled (a well-formed replica never
    /// produces such a value).
    pub fn rows(&self, sub: &str) -> Result<JsValue, JsValue> {
        to_js(&self.replica.rows_of(&Sub::new(sub)))
    }

    /// The scalar value of subscription `sub`, or `null` for any other shape.
    ///
    /// # Errors
    /// A JS error only if the value cannot be marshaled.
    pub fn scalar(&self, sub: &str) -> Result<JsValue, JsValue> {
        to_js(&self.replica.scalar_of(&Sub::new(sub)))
    }

    /// The frontier token subscription `sub` was last observed at, if any.
    #[must_use]
    pub fn frontier(&self, sub: &str) -> Option<String> {
        self.replica.frontier_of(&Sub::new(sub)).map(|ft| ft.into_inner())
    }

    /// Whether subscription `sub` has terminated (closed or reset).
    #[wasm_bindgen(js_name = isClosed)]
    #[must_use]
    pub fn is_closed(&self, sub: &str) -> bool {
        self.replica.is_closed(&Sub::new(sub))
    }

    /// The reason subscription `sub` closed, if it did (e.g. `unauthorized`).
    #[wasm_bindgen(js_name = closeReason)]
    #[must_use]
    pub fn close_reason(&self, sub: &str) -> Option<String> {
        self.replica
            .close_reason_of(&Sub::new(sub))
            .and_then(|reason| serde_json::to_value(reason).ok())
            .and_then(|value| value.as_str().map(str::to_owned))
    }

    /// The subscriptions the replica is tracking, in order.
    #[must_use]
    pub fn subs(&self) -> Vec<String> {
        self.replica.subs().into_iter().map(Sub::into_inner).collect()
    }

    /// The connection frontier last observed across all subscriptions, if any.
    #[wasm_bindgen(js_name = connectionFrontier)]
    #[must_use]
    pub fn connection_frontier(&self) -> Option<String> {
        self.replica.connection_frontier().map(|ft| ft.as_str().to_owned())
    }
}

impl Default for WireClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A per-client §12.3 operation capability. The shell seeds it with a high-entropy id
/// (e.g. `crypto.randomUUID()`) — the wasm core introduces no randomness — and this
/// handle carries it: the `id` is the `Liasse-Operation-Id` header on a `call`, and
/// [`OperationHandle::status_frame`] is the body of an `operation` status query.
#[wasm_bindgen]
pub struct OperationHandle {
    id: OperationId,
}

#[wasm_bindgen]
impl OperationHandle {
    /// Carry a client-generated operation id.
    ///
    /// # Errors
    /// A JS error if `id` is empty — an operation capability must be non-empty.
    #[wasm_bindgen(constructor)]
    pub fn new(id: String) -> Result<OperationHandle, JsValue> {
        if id.is_empty() {
            return Err(JsValue::from_str("operation id must be non-empty"));
        }
        Ok(Self { id: OperationId::new(id) })
    }

    /// The operation capability, for the `Liasse-Operation-Id` request header.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn id(&self) -> String {
        self.id.as_str().to_owned()
    }

    /// Encode the `operation` status-query body (§12.3) for this capability.
    ///
    /// # Errors
    /// A JS error if the request cannot be rendered.
    #[wasm_bindgen(js_name = statusFrame)]
    pub fn status_frame(&self) -> Result<String, JsValue> {
        request::operation(self.id.as_str()).map_err(core_err)
    }
}

/// Encode a `hello` request body (§11).
///
/// # Errors
/// A JS error if `auth`/`context` is not JSON-serializable or the body cannot render.
#[wasm_bindgen(js_name = encodeHello)]
pub fn encode_hello(auth: JsValue, context: JsValue) -> Result<String, JsValue> {
    request::hello(opt_value(auth)?, opt_value(context)?).map_err(core_err)
}

/// Encode a `manifest` request body (§12.1).
///
/// # Errors
/// A JS error if the body cannot render.
#[wasm_bindgen(js_name = encodeManifest)]
pub fn encode_manifest() -> Result<String, JsValue> {
    request::manifest().map_err(core_err)
}

/// Encode a `view` request body (§12.2). `params`/`window`/`auth`/`context` are
/// `null`/`undefined` when absent.
///
/// # Errors
/// A JS error if an argument is not JSON-serializable, `window` is malformed, or the
/// body cannot render.
#[wasm_bindgen(js_name = encodeView)]
pub fn encode_view(
    sub: &str,
    address: &str,
    params: JsValue,
    window: JsValue,
    auth: JsValue,
    context: JsValue,
) -> Result<String, JsValue> {
    request::view(sub, address, opt_value(params)?, opt_window(window)?, opt_value(auth)?, opt_value(context)?)
        .map_err(core_err)
}

/// Encode an `unsubscribe` request body (§12.2).
///
/// # Errors
/// A JS error if the body cannot render.
#[wasm_bindgen(js_name = encodeUnsubscribe)]
pub fn encode_unsubscribe(sub: &str) -> Result<String, JsValue> {
    request::unsubscribe(sub).map_err(core_err)
}

/// Encode a `call` request body (§10, §12.3). `args` defaults to `null` when
/// `null`/`undefined`.
///
/// # Errors
/// A JS error if an argument is not JSON-serializable or the body cannot render.
#[wasm_bindgen(js_name = encodeCall)]
pub fn encode_call(address: &str, args: JsValue, auth: JsValue, context: JsValue) -> Result<String, JsValue> {
    let args = opt_value(args)?.unwrap_or(Value::Null);
    request::call(address, args, opt_value(auth)?, opt_value(context)?).map_err(core_err)
}

/// Encode a `fetch` request body (§12.1).
///
/// # Errors
/// A JS error if `params` is not JSON-serializable or the body cannot render.
#[wasm_bindgen(js_name = encodeFetch)]
pub fn encode_fetch(address: &str, params: JsValue) -> Result<String, JsValue> {
    request::fetch(address, opt_value(params)?).map_err(core_err)
}

/// Encode an `operation` status-query body (§12.3) for capability `id`.
///
/// # Errors
/// A JS error if the body cannot render.
#[wasm_bindgen(js_name = encodeOperation)]
pub fn encode_operation(id: &str) -> Result<String, JsValue> {
    request::operation(id).map_err(core_err)
}

/// Split an SSE text stream into its events (`[{ event?, id?, data, retry? }]`), for a
/// shell reading a raw byte stream rather than a native `EventSource`.
///
/// # Errors
/// A JS error only if the events cannot be marshaled (parsing itself never fails).
#[wasm_bindgen(js_name = parseSse)]
pub fn parse_sse(text: &str) -> Result<JsValue, JsValue> {
    to_js(&request::parse_sse(text))
}

/// Marshal any serializable value to a `JsValue` through JSON text, preserving
/// arbitrary-precision numbers.
fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let text = serde_json::to_string(value).map_err(marshal_err)?;
    JSON::parse(&text)
}

/// Read a JS value into an opaque wire [`Value`] through JSON text.
fn from_js(js: &JsValue) -> Result<Value, JsValue> {
    let text = JSON::stringify(js).map_err(|_| JsValue::from_str("value is not JSON-serializable"))?;
    serde_json::from_str(&String::from(text)).map_err(|error| JsValue::from_str(&format!("invalid value: {error}")))
}

/// `null`/`undefined` become `None`; anything else is read as a wire value.
fn opt_value(js: JsValue) -> Result<Option<Value>, JsValue> {
    if js.is_null() || js.is_undefined() {
        Ok(None)
    } else {
        Ok(Some(from_js(&js)?))
    }
}

/// `null`/`undefined` become `None`; anything else is read as a bounded window.
fn opt_window(js: JsValue) -> Result<Option<WireWindow>, JsValue> {
    match opt_value(js)? {
        None => Ok(None),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(|error| JsValue::from_str(&format!("invalid window: {error}"))),
    }
}

fn core_err(error: CoreError) -> JsValue {
    JsValue::from_str(&error.to_string())
}

fn marshal_err(error: serde_json::Error) -> JsValue {
    JsValue::from_str(&format!("marshal error: {error}"))
}
