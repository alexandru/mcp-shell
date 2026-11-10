use std::{
    io,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use rmcp::{
    ErrorData as McpError,
    model::{CallToolRequestParams, CallToolResponse, RequestId},
};
use serde::Serialize;
use serde_json::json;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
};
use tracing::Span;

use crate::execution::{ExecutionOutput, Termination};

pub(crate) struct ToolInvocation {
    pub(crate) span: Span,
    started: Option<Instant>,
}

impl ToolInvocation {
    pub(crate) fn start(request: &CallToolRequestParams, request_id: &RequestId) -> Self {
        let span = tracing::info_span!(
            "tool_call",
            invocation_id = NEXT_INVOCATION_ID.fetch_add(1, Ordering::Relaxed),
            request_id = %request_id,
            tool = %request.name,
        );
        span.in_scope(|| {
            tracing::info!(event = "tool_call_started", request = %json!(request));
        });
        Self {
            span,
            started: Some(Instant::now()),
        }
    }

    pub(crate) fn finish(mut self, result: &Result<CallToolResponse, McpError>) {
        if let Some(started) = self.started.take() {
            self.span.in_scope(|| {
                let response = match result {
                    Ok(CallToolResponse::Complete(result)) => json!({
                        "result": {
                            "isError": result.is_error.unwrap_or(false),
                            "error": result.structured_content.as_ref().and_then(|value| value.get("error")),
                        }
                    }),
                    Ok(CallToolResponse::InputRequired(_)) => json!({"status": "input_required"}),
                    Ok(CallToolResponse::Task(_)) => json!({"status": "task_created"}),
                    Ok(_) => json!({"status": "pending"}),
                    Err(error) => json!({"error": error}),
                };
                tracing::info!(
                    event = "tool_call_finished",
                    elapsed_seconds = started.elapsed().as_secs_f64(),
                    response = %response,
                );
            });
        }
    }
}

impl Drop for ToolInvocation {
    fn drop(&mut self) {
        if let Some(started) = self.started.take() {
            self.span.in_scope(|| {
                tracing::warn!(
                    event = "tool_call_cancelled",
                    elapsed_seconds = started.elapsed().as_secs_f64(),
                    "tool request ended before a result was available",
                );
            });
        }
    }
}

pub(crate) async fn output_summary(output: &mut ExecutionOutput) {
    let summary = async {
        let stdout = summarize(&mut output.stdout).await?;
        let stderr = summarize(&mut output.stderr).await?;
        Ok::<_, io::Error>((stdout, stderr))
    }
    .await;
    match summary {
        Ok((stdout, stderr)) => {
            let termination = match &output.termination {
                Termination::Exited { exit_code } => json!({"exit_code": exit_code}),
                Termination::Signalled { signal } => json!({"signal": signal}),
            };
            tracing::info!(
                event = "tool_output",
                response = %json!({
                    "termination": termination,
                    "stdout": stdout,
                    "stderr": stderr,
                }),
            );
        }
        Err(error) => tracing::warn!(event = "tool_output_summary_failed", error = %error),
    }
}

static NEXT_INVOCATION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize)]
struct ByteCount(u64);

const PREVIEW_BYTES: ByteCount = ByteCount(512);

#[derive(Serialize)]
struct StreamSummary {
    bytes: ByteCount,
    preview: String,
    truncated: bool,
}

async fn summarize(file: &mut File) -> io::Result<StreamSummary> {
    let bytes = ByteCount(file.metadata().await?.len());
    let mut preview = Vec::new();
    let read = (&mut *file)
        .take(PREVIEW_BYTES.0)
        .read_to_end(&mut preview)
        .await;
    // Output delivery reads the same file after the summary.
    file.rewind().await?;
    read?;
    Ok(StreamSummary {
        truncated: bytes.0 > PREVIEW_BYTES.0,
        bytes,
        preview: String::from_utf8_lossy(&preview).into_owned(),
    })
}
