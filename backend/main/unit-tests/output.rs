use std::{
    error::Error,
    io::{self, Seek, SeekFrom, Write},
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::{
    execution::{ExecutionOutput, Termination},
    output::{CHUNK_SIZE, stream},
};

#[tokio::test]
async fn response_matches_serde_json_for_empty_text_unicode_and_termination()
-> Result<(), Box<dyn Error>> {
    let cases = [
        (
            Vec::new(),
            Vec::new(),
            Termination::Exited { exit_code: 0 },
            json!({"exit_code": 0}),
        ),
        (
            "quote \" slash \\ tab\t newline\n nul\0 unicode ✓"
                .as_bytes()
                .to_vec(),
            b"stderr text".to_vec(),
            Termination::Exited { exit_code: 7 },
            json!({"exit_code": 7}),
        ),
        (
            {
                let mut bytes = vec![b'a'; CHUNK_SIZE.0 - 1];
                bytes.extend_from_slice(&[0xe2, 0x9c, 0x93]);
                bytes
            },
            b"x\xffy\xf0\x9f".to_vec(),
            Termination::Signalled { signal: 15 },
            json!({"signal": 15}),
        ),
    ];

    for (stdout, stderr, termination, expected_termination) in cases {
        let (actual, _) = collect_response(&stdout, &stderr, termination).await?;
        let expected = expected_response(&stdout, &stderr, expected_termination)?;
        assert_response_eq(&actual, &expected);
    }

    Ok(())
}

#[tokio::test]
async fn large_output_is_yielded_in_bounded_chunks() -> Result<(), Box<dyn Error>> {
    let stdout = vec![b'x'; 128 * 1024];
    let stderr = vec![b'y'; 96 * 1024];
    let (actual, chunks) =
        collect_response(&stdout, &stderr, Termination::Exited { exit_code: 0 }).await?;
    let expected = expected_response(&stdout, &stderr, json!({"exit_code": 0}))?;

    assert_response_eq(&actual, &expected);
    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|size| *size <= CHUNK_SIZE.0));
    assert!(chunks.iter().all(|size| *size > 0));
    Ok(())
}

#[tokio::test]
async fn dropping_response_releases_the_execution_permit() -> Result<(), Box<dyn Error>> {
    let semaphore = Arc::new(Semaphore::new(1));
    let permit = semaphore
        .clone()
        .try_acquire_owned()
        .map_err(io::Error::other)?;
    let output = ExecutionOutput {
        stdout: file(&vec![b'x'; 128 * 1024])?,
        stderr: file(b"")?,
        termination: Termination::Exited { exit_code: 0 },
        _permit: permit,
    };
    let mut response = Box::pin(stream(message(), output));

    assert_eq!(semaphore.available_permits(), 0);
    let first = response
        .next()
        .await
        .ok_or_else(|| io::Error::other("response ended before its first chunk"))??;
    assert!(!first.is_empty());
    drop(first);
    drop(response);

    tokio::time::timeout(Duration::from_secs(5), async {
        while semaphore.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(semaphore.available_permits(), 1);
    Ok(())
}

async fn collect_response(
    stdout: &[u8],
    stderr: &[u8],
    termination: Termination,
) -> Result<(Vec<u8>, Vec<usize>), Box<dyn Error>> {
    let response = stream(
        message(),
        ExecutionOutput {
            stdout: file(stdout)?,
            stderr: file(stderr)?,
            termination,
            _permit: Arc::new(Semaphore::new(1))
                .try_acquire_owned()
                .map_err(io::Error::other)?,
        },
    );
    tokio::pin!(response);
    let mut bytes = Vec::new();
    let mut chunks = Vec::new();
    while let Some(chunk) = response.next().await {
        let chunk = chunk?;
        chunks.push(chunk.len());
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, chunks))
}

fn message() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 4,
        "result": {
            "content": [{"type": "text", "text": "{}"}],
            "structuredContent": {},
            "isError": false,
            "resultType": "complete",
            "_meta": {"retained": true}
        }
    })
}

fn expected_response(
    stdout: &[u8],
    stderr: &[u8],
    termination: Value,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut expected_output = json!({
        "stdout": String::from_utf8_lossy(stdout).into_owned(),
        "stderr": String::from_utf8_lossy(stderr).into_owned(),
    });
    let termination = termination
        .as_object()
        .ok_or_else(|| io::Error::other("test termination must be a JSON object"))?;
    let output = expected_output
        .as_object_mut()
        .ok_or_else(|| io::Error::other("test output must be a JSON object"))?;
    output.extend(termination.clone());
    let content_text = serde_json::to_string(&expected_output)?;
    let mut expected_message = message();
    let result = expected_message
        .get_mut("result")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("test JSON message has no result object"))?;
    result.insert(
        "content".to_owned(),
        json!([{"type": "text", "text": content_text}]),
    );
    result.insert("structuredContent".to_owned(), expected_output);

    let mut expected = b"data: ".to_vec();
    expected.extend_from_slice(&serde_json::to_vec(&expected_message)?);
    expected.extend_from_slice(b"\n\n");
    Ok(expected)
}

fn assert_response_eq(actual: &[u8], expected: &[u8]) {
    let first_difference = actual
        .iter()
        .zip(expected)
        .position(|(actual_byte, expected_byte)| actual_byte != expected_byte)
        .or_else(|| (actual.len() != expected.len()).then_some(actual.len().min(expected.len())));
    assert!(
        actual == expected,
        "wire mismatch: actual bytes {}, expected bytes {}, first differing position {:?}",
        actual.len(),
        expected.len(),
        first_difference
    );
}

fn file(content: &[u8]) -> Result<tokio::fs::File, io::Error> {
    let mut file = tempfile::tempfile()?;
    file.write_all(content)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(tokio::fs::File::from_std(file))
}
