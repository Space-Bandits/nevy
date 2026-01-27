//! Promise polling utilities for browser WebTransport.
//!
//! This module provides utilities for polling JavaScript promises without
//! requiring an async runtime in the game loop.

use std::{cell::RefCell, rc::Rc};

use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// A pending promise that can be polled for completion.
///
/// This allows asynchronous JavaScript operations to be integrated into
/// a synchronous game loop by polling each frame.
pub struct PendingPromise<T> {
    result: Rc<RefCell<Option<Result<T, JsValue>>>>,
}

impl<T: 'static> PendingPromise<T> {
    /// Create a new pending promise.
    ///
    /// The transform function converts the successful JsValue result into the desired type.
    pub fn new<F>(promise: Promise, transform: F) -> Self
    where
        F: FnOnce(JsValue) -> T + 'static,
    {
        let result = Rc::new(RefCell::new(None));
        let result_clone = result.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let outcome = JsFuture::from(promise).await;
            *result_clone.borrow_mut() = Some(outcome.map(transform));
        });

        Self { result }
    }

    /// Create a pending promise from an already-resolved value.
    pub fn resolved(value: T) -> Self {
        Self {
            result: Rc::new(RefCell::new(Some(Ok(value)))),
        }
    }

    /// Create a pending promise from an error.
    pub fn rejected(error: JsValue) -> Self {
        Self {
            result: Rc::new(RefCell::new(Some(Err(error)))),
        }
    }

    /// Poll the promise for completion.
    ///
    /// Returns `Some(result)` if the promise has completed, `None` if still pending.
    /// Once polled and returned `Some`, subsequent calls will return `None`.
    pub fn poll(&self) -> Option<Result<T, JsValue>> {
        self.result.borrow_mut().take()
    }

    /// Check if the promise has completed without consuming the result.
    pub fn is_ready(&self) -> bool {
        self.result.borrow().is_some()
    }
}

impl<T> Clone for PendingPromise<T> {
    fn clone(&self) -> Self {
        Self {
            result: self.result.clone(),
        }
    }
}

/// A pending read operation on a stream.
pub struct PendingRead {
    result: Rc<RefCell<Option<Result<Option<Vec<u8>>, JsValue>>>>,
}

impl PendingRead {
    /// Create a new pending read from a readable stream reader.
    pub fn new(reader: web_sys::ReadableStreamDefaultReader) -> Self {
        let result = Rc::new(RefCell::new(None));
        let result_clone = result.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let outcome = JsFuture::from(reader.read()).await;
            let processed = outcome.map(|value| {
                let done = js_sys::Reflect::get(&value, &"done".into())
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                if done {
                    None
                } else {
                    js_sys::Reflect::get(&value, &"value".into())
                        .ok()
                        .and_then(|v| {
                            let array = js_sys::Uint8Array::new(&v);
                            Some(array.to_vec())
                        })
                }
            });
            *result_clone.borrow_mut() = Some(processed);
        });

        Self { result }
    }

    /// Poll the read for completion.
    pub fn poll(&self) -> Option<Result<Option<Vec<u8>>, JsValue>> {
        self.result.borrow_mut().take()
    }

    /// Check if the read has completed.
    pub fn is_ready(&self) -> bool {
        self.result.borrow().is_some()
    }
}

impl Clone for PendingRead {
    fn clone(&self) -> Self {
        Self {
            result: self.result.clone(),
        }
    }
}

/// A pending write operation.
pub struct PendingWrite {
    result: Rc<RefCell<Option<Result<(), JsValue>>>>,
}

impl PendingWrite {
    /// Create a new pending write.
    pub fn new(writer: web_sys::WritableStreamDefaultWriter, data: &[u8]) -> Self {
        let result = Rc::new(RefCell::new(None));
        let result_clone = result.clone();

        let array = js_sys::Uint8Array::from(data);

        wasm_bindgen_futures::spawn_local(async move {
            let outcome = JsFuture::from(writer.write_with_chunk(&array)).await;
            *result_clone.borrow_mut() = Some(outcome.map(|_| ()));
        });

        Self { result }
    }

    /// Poll the write for completion.
    pub fn poll(&self) -> Option<Result<(), JsValue>> {
        self.result.borrow_mut().take()
    }

    /// Check if the write has completed.
    pub fn is_ready(&self) -> bool {
        self.result.borrow().is_some()
    }
}

impl Clone for PendingWrite {
    fn clone(&self) -> Self {
        Self {
            result: self.result.clone(),
        }
    }
}
