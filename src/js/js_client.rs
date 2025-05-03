use wasm_bindgen::prelude::wasm_bindgen;
use crate::protocol::client::{ComelitClient, ComelitOptions};

#[wasm_bindgen]
pub struct JsComelitClient {
    inner: ComelitClient
}

// #[wasm_bindgen]
// impl  JsComelitClient {
//     #[wasm_bindgen(constructor)]
//     pub fn new(options: ComelitOptions, function: &Function) -> Self {
//         Self {
//             inner: ComelitClient::new()
//         }
//     }
// }