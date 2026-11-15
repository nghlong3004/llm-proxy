//! Bounded body reads for native outbound HTTP adapters.
use crate::domain::error::ProxyError;
use futures_util::StreamExt;
use std::time::Duration;

pub async fn read_bounded(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ProxyError> {
    let mut out = vec![];
    let mut stream = response.bytes_stream();
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(300), stream.next())
        .await
        .map_err(|_| ProxyError::transport())?
    {
        let bytes = bytes.map_err(|_| ProxyError::transport())?;
        if out.len().saturating_add(bytes.len()) > limit {
            return Err(ProxyError::protocol("Upstream body exceeds size limit"));
        }
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}
