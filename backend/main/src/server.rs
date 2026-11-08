use std::{io, sync::Arc};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, JsonObject,
        ListToolsResult, PaginatedRequestParams, ProgressNotificationParam, ServerCapabilities,
        ServerConfig, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{Config, RuntimeLimit},
    execution::Executor,
    policy,
};

pub(crate) async fn serve(config: Config) -> Result<(), ServerError> {
    let mut interrupt = signal(SignalKind::interrupt()).map_err(ServerError::Signal)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(ServerError::Signal)?;
    let listener = tokio::net::TcpListener::bind(config.bind.get())
        .await
        .map_err(ServerError::Bind)?;
    let address = listener.local_addr().map_err(ServerError::Bind)?;
    let config = Arc::new(config);
    let shutdown = CancellationToken::new();
    let executor = Executor::new(config.clone(), shutdown.clone());
    let handler = CommandServer {
        config: config.clone(),
        executor: executor.clone(),
    };
    let transport = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_stateless_protocol_metadata_required(true)
            .with_sse_keep_alive(Some(std::time::Duration::from_secs(1)))
            .with_cancellation_token(shutdown.child_token()),
    );
    let app = Router::new()
        .route_service("/mcp", transport)
        .layer(middleware::from_fn_with_state(config, protect));
    eprintln!("mcp-shell listening on {address}");
    let signal_shutdown = shutdown.clone();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = interrupt.recv() => (),
                _ = terminate.recv() => (),
            }
            signal_shutdown.cancel();
        })
        .await;
    executor.shutdown().await;
    result.map_err(ServerError::Serve)
}

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("could not register shutdown signals")]
    Signal(#[source] io::Error),
    #[error("could not bind HTTP listener")]
    Bind(#[source] io::Error),
    #[error("HTTP server failed")]
    Serve(#[source] io::Error),
}

#[derive(Clone)]
struct CommandServer {
    config: Arc<Config>,
    executor: Executor,
}

impl ServerHandler for CommandServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("mcp-shell", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut schema = JsonObject::new();
        schema.insert("type".into(), json!("object"));
        schema.insert(
            "properties".into(),
            json!({
                "command": {"type": "string", "minLength": 1},
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 86_400_000,
                    "description": "Optional runtime in milliseconds (at most 24 hours), capped by the configured server limit."
                }
            }),
        );
        schema.insert("required".into(), json!(["command"]));
        schema.insert("additionalProperties".into(), json!(false));
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "execute",
            "Execute one authorized Bash-compatible shell expression.",
            schema,
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if request.name.as_ref() != "execute" {
            return Err(McpError::invalid_params("unknown tool", None));
        }
        let arguments = request
            .arguments
            .ok_or_else(|| McpError::invalid_params("expected command argument", None))?;
        let params: ExecuteParams =
            serde_json::from_value(Value::Object(arguments)).map_err(|_| {
                McpError::invalid_params("expected command and optional valid timeout_ms", None)
            })?;
        let expression = match policy::authorize(&params.command, &self.config.allow_rules) {
            Ok(expression) => expression,
            Err(error) => return Ok(tool_error(&error.to_string())),
        };
        if let Some(progress_token) = context.meta.get_progress_token() {
            context
                .peer
                .notify_progress(
                    ProgressNotificationParam::new(progress_token, 0.0)
                        .with_message("execution accepted"),
                )
                .await
                .map_err(|_| {
                    McpError::internal_error("could not start execution response", None)
                })?;
        }
        match self
            .executor
            .execute(expression, context.ct, params.timeout_ms)
            .await
        {
            Ok(output) => {
                let succeeded = output.succeeded();
                let result = serde_json::to_value(output).map_err(|_| {
                    McpError::internal_error("could not serialize execution output", None)
                })?;
                let result = if succeeded {
                    CallToolResult::structured(result)
                } else {
                    CallToolResult::structured_error(result)
                };
                Ok(result.into())
            }
            Err(error) => Ok(tool_error(&error.to_string())),
        }
    }
}

type CommandText = String;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteParams {
    command: CommandText,
    timeout_ms: Option<RuntimeLimit>,
}

struct RequestLimit(usize);
const REQUEST_LIMIT: RequestLimit = RequestLimit(65_536);

async fn protect(State(config): State<Arc<Config>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if headers.get_all(header::HOST).iter().count() != 1 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let valid_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            config
                .allowed_hosts
                .iter()
                .any(|allowed| host.eq_ignore_ascii_case(allowed.as_str()))
        });
    if !valid_host {
        return StatusCode::FORBIDDEN.into_response();
    }
    if headers.get_all(header::ORIGIN).iter().count() > 1 {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let valid_origin = origin.to_str().ok().is_some_and(|origin| {
            config
                .allowed_origins
                .iter()
                .any(|allowed| origin == allowed.as_str())
        });
        if !valid_origin {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let authenticated = headers.get_all(header::AUTHORIZATION).iter().count() == 1
        && headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
            .is_some_and(|(scheme, token)| {
                scheme.eq_ignore_ascii_case("Bearer")
                    && !token.is_empty()
                    && !token.bytes().any(|byte| byte.is_ascii_whitespace())
                    && config.token_hash.verify(token.as_bytes())
            });
    if !authenticated {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer realm=\"mcp-shell\"")],
        )
            .into_response();
    }
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, REQUEST_LIMIT.0).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    next.run(Request::from_parts(parts, Body::from(body))).await
}

fn tool_error(message: &str) -> CallToolResponse {
    CallToolResult::structured_error(json!({"error": message})).into()
}
