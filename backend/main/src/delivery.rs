use std::{io, sync::Arc};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::Response,
};
use futures_util::{Stream, StreamExt, TryStreamExt};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::Mutex,
};
use tokio_util::{io::StreamReader, sync::CancellationToken};

use crate::{execution::ExecutionOutput, output};

/// Carries file-backed output from an SDK handler to its HTTP response body.
#[derive(Clone, Default)]
pub(crate) struct DeferredOutput(Arc<Mutex<Option<ExecutionOutput>>>);

impl DeferredOutput {
    pub(crate) async fn store(&self, output: ExecutionOutput) {
        *self.0.lock().await = Some(output);
    }

    async fn take(&self) -> Option<ExecutionOutput> {
        self.0.lock().await.take()
    }
}

/// Retains the SDK's validation, dispatch, and notifications, and streams its final output.
pub(crate) async fn stream_result(
    State(shutdown): State<CancellationToken>,
    mut request: Request,
    next: Next,
) -> Response {
    let output = DeferredOutput::default();
    request.extensions_mut().insert(output.clone());
    let response = next.run(request).await;
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"))
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    parts
        .headers
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Response::from_parts(
        parts,
        Body::from_stream(replace_result(body, output, shutdown)),
    )
}

fn replace_result(
    body: Body,
    output: DeferredOutput,
    shutdown: CancellationToken,
) -> impl Stream<Item = Result<axum::body::Bytes, io::Error>> + Send {
    async_stream::try_stream! {
        let reader = StreamReader::new(body.into_data_stream().map_err(io::Error::other));
        let mut reader = BufReader::new(reader);
        loop {
            // Only small SDK messages pass here. Command output bypasses SDK serialization.
            let mut line = Vec::new();
            let mut bounded = (&mut reader).take(SDK_LINE_LIMIT.0);
            let count = tokio::select! {
                count = bounded.read_until(b'\n', &mut line) => count,
                _ = shutdown.cancelled() => break,
            }?;
            if count == 0 {
                break;
            }
            if line.last() != Some(&b'\n') {
                Err(io::Error::other("incomplete or oversized SDK event line"))?;
            }
            if let Some(data) = line.strip_prefix(b"data:")
                && let Ok(message) = serde_json::from_slice::<Value>(data)
                && message.get("result").is_some()
                && let Some(captured) = output.take().await
            {
                // rmcp owns protocol envelopes; only file-backed result serialization differs.
                let encoded = output::stream(message, captured);
                futures_util::pin_mut!(encoded);
                loop {
                    let chunk = tokio::select! {
                        chunk = encoded.next() => chunk,
                        _ = shutdown.cancelled() => None,
                    };
                    match chunk {
                        Some(chunk) => yield chunk?,
                        None => break,
                    }
                }
                break;
            }
            yield axum::body::Bytes::from(line);
        }
    }
}

struct SdkLineBytes(u64);
const SDK_LINE_LIMIT: SdkLineBytes = SdkLineBytes(1_048_576);
