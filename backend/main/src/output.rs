use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    str,
    sync::Arc,
};

use axum::body::Bytes;
use futures_util::Stream;
use serde_json::Value;

use crate::execution::{ExecutionOutput, Termination};

/// Streams one complete tool response as an SSE event with bounded output chunks.
pub(crate) fn stream(
    message: Value,
    output: ExecutionOutput,
) -> impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static {
    async_stream::try_stream! {
        let ExecutionOutput {
            stdout,
            stderr,
            termination,
            _permit,
        } = output;
        let mut stdout = stdout.into_std().await;
        let mut stderr = stderr.into_std().await;
        let _permit = Arc::new(_permit);
        let producer_permit = _permit.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        let writer = tokio::task::spawn_blocking(move || {
            let _producer_permit = producer_permit;
            let mut writer = ChunkWriter::new(sender);
            write_sse_message(&mut writer, &message, &mut stdout, &mut stderr, &termination)?;
            writer.flush()
        });

        while let Some(chunk) = receiver.recv().await {
            yield chunk;
        }

        writer
            .await
            .map_err(|error| io::Error::other(error.to_string()))??;
        drop(_permit);
    }
}

struct BufferBytes(usize);

const CHUNK_SIZE: BufferBytes = BufferBytes(8 * 1024);
const CHANNEL_CAPACITY: usize = 2;
const UTF8_CARRY_CAPACITY: BufferBytes = BufferBytes(3);

fn write_sse_message(
    writer: &mut impl Write,
    message: &Value,
    stdout: &mut impl ReadSeek,
    stderr: &mut impl ReadSeek,
    termination: &Termination,
) -> io::Result<()> {
    writer.write_all(b"data: ")?;
    write_rpc_message(writer, message, stdout, stderr, termination)?;
    writer.write_all(b"\n\n")
}

trait ReadSeek: Read + Seek {}

impl<T: Read + Seek> ReadSeek for T {}

fn write_rpc_message(
    writer: &mut impl Write,
    message: &Value,
    stdout: &mut impl ReadSeek,
    stderr: &mut impl ReadSeek,
    termination: &Termination,
) -> io::Result<()> {
    let object = message
        .as_object()
        .ok_or_else(|| invalid_message("final JSON-RPC response must be an object"))?;
    writer.write_all(b"{")?;
    let mut first = true;
    let mut result_written = false;
    for (key, value) in object {
        if !first {
            writer.write_all(b",")?;
        }
        first = false;
        serde_json::to_writer(&mut *writer, key).map_err(json_error)?;
        writer.write_all(b":")?;
        if key == "result" {
            write_tool_result(writer, value, stdout, stderr, termination)?;
            result_written = true;
        } else {
            serde_json::to_writer(&mut *writer, value).map_err(json_error)?;
        }
    }
    if !result_written {
        return Err(invalid_message("final JSON-RPC response has no result"));
    }
    writer.write_all(b"}")
}

fn write_tool_result(
    writer: &mut impl Write,
    result: &Value,
    stdout: &mut impl ReadSeek,
    stderr: &mut impl ReadSeek,
    termination: &Termination,
) -> io::Result<()> {
    let object = result
        .as_object()
        .ok_or_else(|| invalid_message("final tool result must be an object"))?;
    writer.write_all(b"{")?;
    let mut first = true;
    let mut content_written = false;
    let mut structured_written = false;
    for (key, value) in object {
        if !first {
            writer.write_all(b",")?;
        }
        first = false;
        serde_json::to_writer(&mut *writer, key).map_err(json_error)?;
        writer.write_all(b":")?;
        match key.as_str() {
            "content" => {
                write_fallback_content(writer, stdout, stderr, termination)?;
                content_written = true;
            }
            "structuredContent" => {
                write_output_object(writer, stdout, stderr, termination)?;
                structured_written = true;
            }
            _ => serde_json::to_writer(&mut *writer, value).map_err(json_error)?,
        }
    }
    if !content_written {
        write_field_separator(writer, &mut first)?;
        serde_json::to_writer(&mut *writer, "content").map_err(json_error)?;
        writer.write_all(b":")?;
        write_fallback_content(writer, stdout, stderr, termination)?;
    }
    if !structured_written {
        write_field_separator(writer, &mut first)?;
        serde_json::to_writer(&mut *writer, "structuredContent").map_err(json_error)?;
        writer.write_all(b":")?;
        write_output_object(writer, stdout, stderr, termination)?;
    }
    writer.write_all(b"}")
}

fn write_fallback_content(
    writer: &mut impl Write,
    stdout: &mut impl ReadSeek,
    stderr: &mut impl ReadSeek,
    termination: &Termination,
) -> io::Result<()> {
    writer.write_all(b"[{\"text\":\"")?;
    let mut nested = JsonStringWriter(writer);
    write_output_object(&mut nested, stdout, stderr, termination)?;
    nested.0.write_all(b"\",\"type\":\"text\"}]")
}

fn write_output_object(
    writer: &mut impl Write,
    stdout: &mut impl ReadSeek,
    stderr: &mut impl ReadSeek,
    termination: &Termination,
) -> io::Result<()> {
    match termination {
        Termination::Exited { exit_code } => {
            writer.write_all(b"{\"exit_code\":")?;
            serde_json::to_writer(&mut *writer, exit_code).map_err(json_error)?;
        }
        Termination::Signalled { signal } => {
            writer.write_all(b"{\"signal\":")?;
            serde_json::to_writer(&mut *writer, signal).map_err(json_error)?;
        }
    }
    writer.write_all(b",\"stderr\":")?;
    write_file_json_string(writer, stderr)?;
    writer.write_all(b",\"stdout\":")?;
    write_file_json_string(writer, stdout)?;
    writer.write_all(b"}")
}

fn write_file_json_string(writer: &mut impl Write, file: &mut impl ReadSeek) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    writer.write_all(b"\"")?;
    let mut escaped = JsonStringWriter(writer);
    write_file_lossy_utf8(&mut escaped, file)?;
    escaped.0.write_all(b"\"")
}

fn write_file_lossy_utf8(writer: &mut impl Write, file: &mut impl Read) -> io::Result<()> {
    let mut read_buffer = [0_u8; CHUNK_SIZE.0];
    let mut combined = [0_u8; CHUNK_SIZE.0 + UTF8_CARRY_CAPACITY.0];
    let mut carry = [0_u8; UTF8_CARRY_CAPACITY.0];
    let mut carry_len = 0;

    loop {
        let count = file.read(&mut read_buffer)?;
        if count == 0 {
            if carry_len > 0 {
                writer.write_all("�".as_bytes())?;
            }
            return Ok(());
        }

        let carry_bytes = carry
            .get(..carry_len)
            .ok_or_else(|| invalid_message("invalid UTF-8 carry length"))?;
        combined
            .get_mut(..carry_len)
            .ok_or_else(|| invalid_message("invalid UTF-8 carry length"))?
            .copy_from_slice(carry_bytes);
        let read_bytes = read_buffer
            .get(..count)
            .ok_or_else(|| invalid_message("invalid file read length"))?;
        let input_end = carry_len
            .checked_add(count)
            .ok_or_else(|| invalid_message("file read length overflowed"))?;
        combined
            .get_mut(carry_len..input_end)
            .ok_or_else(|| invalid_message("invalid combined UTF-8 length"))?
            .copy_from_slice(read_bytes);
        let mut remaining = combined
            .get(..input_end)
            .ok_or_else(|| invalid_message("invalid combined UTF-8 length"))?;
        carry_len = 0;

        loop {
            match str::from_utf8(remaining) {
                Ok(text) => {
                    writer.write_all(text.as_bytes())?;
                    break;
                }
                Err(error) => {
                    let valid_len = error.valid_up_to();
                    let valid = remaining
                        .get(..valid_len)
                        .ok_or_else(|| invalid_message("invalid UTF-8 prefix length"))?;
                    writer.write_all(valid)?;
                    match error.error_len() {
                        Some(invalid_len) => {
                            writer.write_all("�".as_bytes())?;
                            let invalid_end = valid_len
                                .checked_add(invalid_len)
                                .ok_or_else(|| invalid_message("invalid UTF-8 error length"))?;
                            remaining = remaining
                                .get(invalid_end..)
                                .ok_or_else(|| invalid_message("invalid UTF-8 error length"))?;
                        }
                        None => {
                            let incomplete = remaining
                                .get(valid_len..)
                                .ok_or_else(|| invalid_message("invalid UTF-8 carry length"))?;
                            carry
                                .get_mut(..incomplete.len())
                                .ok_or_else(|| invalid_message("invalid UTF-8 carry length"))?
                                .copy_from_slice(incomplete);
                            carry_len = incomplete.len();
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn write_field_separator(writer: &mut impl Write, first: &mut bool) -> io::Result<()> {
    if !*first {
        writer.write_all(b",")?;
    }
    *first = false;
    Ok(())
}

fn invalid_message(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn json_error(error: serde_json::Error) -> io::Error {
    io::Error::other(error)
}

struct ChunkWriter {
    sender: tokio::sync::mpsc::Sender<Bytes>,
    buffer: Vec<u8>,
}

impl ChunkWriter {
    fn new(sender: tokio::sync::mpsc::Sender<Bytes>) -> Self {
        Self {
            sender,
            buffer: Vec::with_capacity(CHUNK_SIZE.0),
        }
    }

    fn send_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(CHUNK_SIZE.0));
        self.sender
            .blocking_send(Bytes::from(buffer))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "response stream was dropped"))
    }
}

impl Write for ChunkWriter {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let original_len = input.len();
        while !input.is_empty() {
            let available = CHUNK_SIZE
                .0
                .checked_sub(self.buffer.len())
                .ok_or_else(|| invalid_message("response buffer exceeded its limit"))?;
            let count = available.min(input.len());
            self.buffer.extend_from_slice(
                input
                    .get(..count)
                    .ok_or_else(|| invalid_message("invalid response input length"))?,
            );
            input = input
                .get(count..)
                .ok_or_else(|| invalid_message("invalid response input length"))?;
            if self.buffer.len() == CHUNK_SIZE.0 {
                self.send_buffer()?;
            }
        }
        Ok(original_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_buffer()
    }
}

struct JsonStringWriter<'a, W: Write>(&'a mut W);

impl<W: Write> Write for JsonStringWriter<'_, W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let mut segment_start = 0;
        for (index, byte) in input.iter().copied().enumerate() {
            let escape: Option<&[u8]> = match byte {
                b'"' => Some(b"\\\""),
                b'\\' => Some(b"\\\\"),
                0x08 => Some(b"\\b"),
                b'\t' => Some(b"\\t"),
                b'\n' => Some(b"\\n"),
                0x0c => Some(b"\\f"),
                b'\r' => Some(b"\\r"),
                0x00..=0x1f => None,
                _ => continue,
            };
            let segment = input
                .get(segment_start..index)
                .ok_or_else(|| invalid_message("invalid JSON string segment"))?;
            self.0.write_all(segment)?;
            if let Some(escape) = escape {
                self.0.write_all(escape)?;
            } else {
                let escaped = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    hex_digit(byte >> 4)?,
                    hex_digit(byte & 0x0f)?,
                ];
                self.0.write_all(&escaped)?;
            }
            segment_start = index
                .checked_add(1)
                .ok_or_else(|| invalid_message("JSON string index overflowed"))?;
        }
        let segment = input
            .get(segment_start..)
            .ok_or_else(|| invalid_message("invalid JSON string segment"))?;
        self.0.write_all(segment)?;
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn hex_digit(nibble: u8) -> io::Result<u8> {
    match nibble {
        0..=9 => Ok(b'0' + nibble),
        10..=15 => b'a'
            .checked_add(nibble - 10)
            .ok_or_else(|| invalid_message("hexadecimal digit overflowed")),
        _ => Err(invalid_message("invalid hexadecimal digit")),
    }
}

#[cfg(test)]
#[path = "../unit-tests/output.rs"]
mod tests;
